/**
 * The door: a GraphAr graph attached to the host's DuckDB as a catalog — DuckDB's `ATTACH … AS
 * name` — and the handle that detaches it.
 */

import { GraphArError } from './error.js';
import { type Graph, type Property as GraphProperty, type Stats, graphOf } from './graph.js';
import {
  type AdjacentList,
  type EdgeInfo,
  type GraphInfo,
  type Property,
  type PropertyGroup,
  type VertexInfo,
  edgeName,
  parseEdge,
  parseGraph,
  parseVertex,
} from './info.js';

/**
 * The engine, as this package needs it: one statement in, an answer in columns out — the part of
 * apache-arrow's `Table` read here, so DuckDB-WASM's answer is one as it comes. `@fossil-lang/types`'
 * `Engine` and `@kanzo-tech/mosaic`'s engine satisfy it structurally.
 */
export interface Engine {
  query(sql: string, options: { readonly signal: AbortSignal }): Promise<Table>;
}

/** An answer in columns. */
export interface Table {
  readonly numRows: number;
  /** One column by name, or `null` when the answer has none of that name. */
  getChild(name: string): { get(index: number): unknown } | null;
}

/**
 * What SQL calls each file of the graph: handed paths relative to the graph document's
 * directory — `person.vertex.yml`, `vertex/person/id/chunk0` — it answers, in order, a name
 * `read_text`, `read_blob` and `read_parquet` read: a URL, a path, a file registered with the
 * engine. It is called with every file an attach needs at once, a few times per attach.
 */
export type Resolve = (paths: readonly string[]) => readonly string[] | Promise<readonly string[]>;

/** What {@link attachGraph} takes besides the engine, the catalog and the resolver. */
export interface AttachOptions {
  /** The graph document, relative to what {@link Resolve} resolves against: `sample.graph.yml`. */
  readonly entry: string;
  /** Stops the attach: it rejects with the signal's reason, and what it made is given back. */
  readonly signal?: AbortSignal;
  /**
   * The DuckDB type of each user-defined type the graph's version declares (`gar/v1 (langString)`),
   * as SQL: `{ langString: 'STRUCT(value VARCHAR, lang VARCHAR)' }`. GraphAr names such types and
   * defines no storage for them, so a view over chunks reads the type from the Parquet file, and
   * this is needed only to type a view that has no chunk to read: a type with no rows.
   */
  readonly userTypes?: Readonly<Record<string, string>>;
}

/** GraphAr's reserved columns: C++ `GeneralParams` (`general_params.h`). */
export const VERTEX_INDEX_COL = '_graphArVertexIndex';
export const SRC_INDEX_COL = '_graphArSrcIndex';
export const DST_INDEX_COL = '_graphArDstIndex';

/** The relations the graph's information files become, beside the views. */
const METADATA = ['graphar_vertex_types', 'graphar_edge_types', 'graphar_properties', 'graphar_extra_info', 'graphar_chunks', 'graphar_column_stats'] as const;

const holders = new WeakMap<Engine, Map<string, number>>();

/** A single-quoted SQL string literal. */
export const lit = (value: string): string => `'${value.replace(/'/g, "''")}'`;
/** A quoted SQL identifier. */
export const ident = (name: string): string => `"${name.replace(/"/g, '""')}"`;
const list = (values: readonly string[]): string => `[${values.map(lit).join(', ')}]`;

/** Prefixes joined as C++'s `BuildPath` joins them: as written, a slash added where one is missing. */
const dir = (...parts: readonly string[]): string => parts.map((p) => (p.endsWith('/') ? p : `${p}/`)).join('');

// The paths, as `graph_info.cc` composes them (`graphar-parquet`'s `paths`, in Rust).
const vertexChunk = (v: VertexInfo, g: PropertyGroup, k: number) => `${dir(v.prefix, g.prefix)}chunk${k}`;
const vertexCount = (v: VertexInfo) => `${dir(v.prefix)}vertex_count`;
const adjListChunk = (e: EdgeInfo, a: AdjacentList, p: number, k: number) => `${dir(e.prefix, a.prefix)}adj_list/part${p}/chunk${k}`;
const edgePropertyChunk = (e: EdgeInfo, a: AdjacentList, g: PropertyGroup, p: number, k: number) =>
  `${dir(e.prefix, a.prefix, g.prefix)}part${p}/chunk${k}`;
const offsetChunk = (e: EdgeInfo, a: AdjacentList, p: number) => `${dir(e.prefix, a.prefix)}offset/chunk${p}`;
const edgeCount = (e: EdgeInfo, a: AdjacentList, p: number) => `${dir(e.prefix, a.prefix)}edge_count${p}`;
const edgeVertexCount = (e: EdgeInfo, a: AdjacentList) => `${dir(e.prefix, a.prefix)}vertex_count`;

/** GraphAr's own data types, as DuckDB's. */
const GRAPHAR_TYPES: Readonly<Record<string, string>> = {
  bool: 'BOOLEAN', int32: 'INTEGER', int64: 'BIGINT', float: 'FLOAT', double: 'DOUBLE', string: 'VARCHAR',
  date: 'DATE', timestamp: 'TIMESTAMP', time: 'TIME',
};

/** GraphAr's type as DuckDB's, for a relation with no file to read its type from. */
function duckType(dataType: string, at: string, userTypes: Readonly<Record<string, string>>): string {
  const list = /^list<(.+)>$/.exec(dataType);
  if (list) return `${duckType(list[1]!, at, userTypes)}[]`;
  if (Object.hasOwn(userTypes, dataType)) return userTypes[dataType]!;
  const t = Object.hasOwn(GRAPHAR_TYPES, dataType) ? GRAPHAR_TYPES[dataType] : undefined;
  if (t === undefined) {
    throw new GraphArError('unsupported', `${at}: data type \`${dataType}\` has no DuckDB type: GraphAr defines none, and \`userTypes\` gives none`);
  }
  return t;
}

const propertyType = (p: Property, at: string, userTypes: Readonly<Record<string, string>>): string =>
  p.cardinality === 'single' ? duckType(p.data_type, at, userTypes) : `${duckType(p.data_type, at, userTypes)}[]`;

/**
 * A relation of these columns and rows, typed even when it has no row — a `VALUES` with none is
 * not SQL, and a column of nulls only has no type.
 */
function relationOf(columns: readonly (readonly [string, string])[], rows: readonly (readonly string[])[]): string {
  const typed = columns.map(([c, t]) => `CAST(${ident(c)} AS ${t}) AS ${ident(c)}`).join(', ');
  const names = columns.map(([c]) => ident(c)).join(', ');
  if (rows.length === 0) return `SELECT ${typed} FROM (SELECT ${columns.map(() => 'NULL').join(', ')}) AS t(${names}) WHERE false`;
  // Each column is cast, so a literal keeps the type declared: an integer literal is not UBIGINT.
  return `SELECT ${typed} FROM (VALUES ${rows.map((r) => `(${r.join(', ')})`).join(', ')}) AS t(${names})`;
}

/** One count file as an `int64` in SQL, or `NULL` when it is not 8 bytes: little-endian, so reversed into hex. */
const COUNT = `CASE WHEN octet_length(content) = 8
  THEN ('0x' || array_to_string(list_reverse(regexp_extract_all(hex(content), '..')), ''))::UBIGINT END`;

/** What reads the graph's files through the engine, by the names `resolve` gives them. */
export interface Reader {
  /** One statement; a refusal becomes `fail(cause)`, an abort rejects with the signal's reason. */
  run(sql: string, fail: (cause: unknown) => GraphArError, signal?: AbortSignal): Promise<Table>;
  /** The names `resolve` gives `paths`, as many as there are paths. */
  names(paths: readonly string[]): Promise<readonly string[]>;
  /** Each file's text, read with `read_text`; a file that does not read is `unreadable`. */
  texts(paths: readonly string[]): Promise<string[]>;
  /** Each count file's `int64`, read with `read_blob`. */
  counts(paths: readonly string[]): Promise<number[]>;
}

/** The {@link Reader} over `engine` and `resolve`, stopped by `signal`. */
export function reader(engine: Engine, resolve: Resolve, signal: AbortSignal | undefined): Reader {
  const quiet = new AbortController().signal;
  const run = async (sql: string, fail: (cause: unknown) => GraphArError, s: AbortSignal | undefined = signal): Promise<Table> => {
    s?.throwIfAborted();
    try {
      return await engine.query(sql, { signal: s ?? quiet });
    } catch (cause) {
      s?.throwIfAborted();
      throw fail(cause);
    }
  };
  const names = async (paths: readonly string[]): Promise<readonly string[]> => {
    const named = await resolve(paths);
    if (named.length !== paths.length) throw new TypeError(`resolve answered ${named.length} names for ${paths.length} paths`);
    return named;
  };
  /** Each file through `fn` in one statement, by the name it was resolved to. */
  const read = async (paths: readonly string[], fn: string, column: string): Promise<unknown[]> => {
    if (paths.length === 0) return [];
    const named = await names(paths);
    const unreadable = (at: string, cause?: unknown) =>
      new GraphArError('unreadable', `${at}: did not read`, cause === undefined ? { document: at } : { document: at, cause });
    const answer = await run(`SELECT filename, ${column} AS v FROM ${fn}(${list(named)})`, (cause) => unreadable(paths.join(', '), cause));
    const byName = new Map<string, unknown>();
    const files = answer.getChild('filename');
    const values = answer.getChild('v');
    for (let i = 0; i < answer.numRows; i++) byName.set(String(files?.get(i)), values?.get(i));
    return named.map((n, i) => {
      if (!byName.has(n)) throw unreadable(paths[i]!);
      return byName.get(n);
    });
  };
  const texts = async (paths: readonly string[]): Promise<string[]> => (await read(paths, 'read_text', 'content')).map(String);
  const counts = async (paths: readonly string[]): Promise<number[]> =>
    (await read(paths, 'read_blob', COUNT)).map((v, i) => {
      const n = v === null || v === undefined ? NaN : Number(v);
      if (!Number.isSafeInteger(n)) throw new GraphArError('invalid', `${paths[i]}: not a count, one int64`, { document: paths[i]! });
      return n;
    });
  return { run, names, texts, counts };
}

/**
 * **Attach a GraphAr graph to the engine as the catalog `catalog`**: a view per vertex type,
 * `"<catalog>"."<type>"`, and per edge type, `"<catalog>"."<src_type>_<edge_type>_<dst_type>"`,
 * with GraphAr's own columns, and the information files as five relations and the chunks' statistics as a sixth;
 * and answer the {@link Graph} over them, which is what a consumer reads: it names every relation
 * and column a query needs, so a consumer writes none of the format's.
 *
 * ```ts
 * await using g = await attachGraph(engine, 'job', (paths) => paths.map((p) => `${url}/${p}`), { entry: 'fossil.graph.yml' });
 * const { sql } = g.match([{ as: 'p', type: 'person' }, { edge: 'created' }, { as: 's', type: 'software' }], { columns: ['p.name', 's.name'] });
 * ```
 *
 * Every file is read through the engine: the YAML with `read_text`, the count files with
 * `read_blob`, the chunks with `read_parquet`, each name given by `resolve`. Nothing is fetched
 * any other way, and nothing but SQL reaches the engine.
 *
 * - A vertex view is `_graphArVertexIndex`, then every property, the groups joined on the id.
 * - An edge view is `_graphArSrcIndex`, `_graphArDstIndex`, then every property, read from the
 *   edge type's first adjacency list — any one holds every edge — with each property chunk joined
 *   to its adjacency-list chunk row for row.
 * - `graphar_vertex_types(type, count, ordinal)`: one row per vertex type; `count` is its
 *   `vertex_count` file; `ordinal` is its place in the graph document's `vertices`, from 1.
 * - `graphar_edge_types(name, src_type, edge_type, dst_type, count, directed, ordinal)`: one row
 *   per edge type, `name` its view's; `count` sums the first adjacency list's `edge_count` files.
 * - `graphar_properties(type, property, data_type, is_primary, is_nullable, ordinal, cardinality)`: one row per
 *   property; `type` is the vertex type or the edge type's `name`; `data_type` is GraphAr's word, a
 *   user-defined type's name as written; `ordinal` is the column's place in the type's view after
 *   GraphAr's id columns, from 1; `cardinality` is `single`, `list` or `set`, and a column of any
 *   other than `single` is a DuckDB list of `data_type`.
 * - `graphar_extra_info(key, value)`: the graph document's `extra_info`, in order.
 * - `graphar_chunks(type, kind, path)`: one row per Parquet chunk file the views read, in the order
 *   vertex type, then edge type; `kind` is `vertex`, `offset`, `adj_list` or `edge_property`; `path` is
 *   exactly the name `resolve` gave the file, so `parquet_metadata(path)` and
 *   `parquet_kv_metadata(path)` read it. Only the edge type's first adjacency list is listed, with
 *   its offset chunks if it is ordered.
 * - `graphar_column_stats(type, property, null_count, min, max, distinct_min, distinct_max)`: one row
 *   per property of a single value and a GraphAr type, read from its chunks' Parquet `Statistics`,
 *   never from a value: `null_count` sums the row groups', `min` and `max` are the least and greatest
 *   in the property's type, as text, and the distinct count is `distinct_min … distinct_max`, the
 *   largest row group's and their sum, which are one number when the property has one row group. A
 *   count some row group does not carry is `NULL` for the property: no value is scanned to make it.
 *
 * A graph `prefix` is read against the graph document's directory, where GraphAr's C++ library
 * reads a relative one against its working directory; without one, the data is under the document,
 * which is GraphAr's default. Only Parquet is read.
 *
 * A vertex type whose internal ids are not continuous — its first group's chunks do not hold
 * `vertex_count` rows numbered `0 … vertex_count − 1` — is `invalid`, naming the type: the
 * specification numbers vertices from 0 by row, and a global id built as a prefix sum of the counts
 * plus this index is injective only without gaps. The check reads the chunks' footers.
 *
 * @throws {GraphArError} before any view is made, for what the files hold (`unreadable`,
 *   `unparseable`, `invalid`, `unsupported`, `duplicate`); `engine` when a view is refused. An abort
 *   rejects with the signal's reason. Either way the catalog is given back.
 */
export async function attachGraph(
  engine: Engine,
  catalog: string,
  resolve: Resolve,
  options: AttachOptions,
): Promise<Graph> {
  const { entry, signal } = options;
  const userTypes = options.userTypes ?? {};
  const { run, names, texts, counts } = reader(engine, resolve, signal);

  // The information files.
  const base = entry.slice(0, entry.lastIndexOf('/') + 1);
  const doc = parseGraph((await texts([entry]))[0]!, entry);
  const infos = await texts([...doc.vertices, ...doc.edges].map((p) => base + p));
  const vertices = doc.vertices.map((p, i) => parseVertex(infos[i]!, p));
  const edges = doc.edges.map((p, i) => parseEdge(infos[vertices.length + i]!, p));
  const graph: GraphInfo = { ...doc.info, vertices, edges };

  const relations = new Set<string>(METADATA);
  for (const name of [...vertices.map((v) => v.type), ...edges.map(edgeName)]) {
    if (relations.has(name)) throw new GraphArError('duplicate', `\`${name}\` names two relations of the graph`);
    relations.add(name);
  }
  const parquet = (at: string, fileType: string) => {
    if (fileType !== 'parquet') throw new GraphArError('unsupported', `${at}: file type \`${fileType}\` is not Parquet`);
  };
  // Only a relation with no chunk to read its types from needs them, so only it asks.
  const types = (at: string, groups: readonly PropertyGroup[]) =>
    groups.flatMap((g) => g.properties.map((p) => [p.name, propertyType(p, at, userTypes)] as const));
  for (const v of vertices) {
    for (const g of v.property_groups) parquet(`vertex \`${v.type}\``, g.file_type);
  }
  for (const e of edges) {
    parquet(`edge \`${edgeName(e)}\`, ${e.adj_lists[0]!.type}`, e.adj_lists[0]!.file_type);
    for (const g of e.property_groups) parquet(`edge \`${edgeName(e)}\``, g.file_type);
  }

  // The counts: every type's, then every part's of each edge type's first adjacency list.
  const root = graph.prefix === undefined ? base : /^([a-z][a-z0-9+.-]*:|\/)/i.test(graph.prefix) ? dir(graph.prefix) : base + dir(graph.prefix);
  const firstCounts = await counts([...vertices.map(vertexCount), ...edges.map((e) => edgeVertexCount(e, e.adj_lists[0]!))].map((p) => root + p));
  const vertexCounts = firstCounts.slice(0, vertices.length);
  const parts = edges.map((e, i) => {
    const a = e.adj_lists[0]!;
    const chunkSize = a.type.endsWith('source') ? e.src_chunk_size : e.dst_chunk_size;
    return Math.ceil(firstCounts[vertices.length + i]! / chunkSize);
  });
  const partPaths = edges.map((e, i) => Array.from({ length: parts[i]! }, (_, p) => root + edgeCount(e, e.adj_lists[0]!, p)));
  const flat = await counts(partPaths.flat());
  let at = 0;
  const partCounts = parts.map((n) => flat.slice(at, (at += n)));

  // Every chunk, named in one call.
  const chunkPaths: string[] = [];
  const vertexFiles = vertices.map((v, i) =>
    v.property_groups.map((g) => Array.from({ length: Math.ceil(vertexCounts[i]! / v.chunk_size) }, (_, k) => chunkPaths.push(root + vertexChunk(v, g, k)) - 1)),
  );
  const edgeFiles = edges.map((e, i) => {
    const a = e.adj_lists[0]!;
    const chunks = partCounts[i]!.flatMap((n, p) => Array.from({ length: Math.ceil(n / e.chunk_size) }, (_, k) => [p, k] as const));
    const offsets = a.type.startsWith('ordered')
      ? partCounts[i]!.map((_, p) => chunkPaths.push(root + offsetChunk(e, a, p)) - 1)
      : [];
    return {
      offsets,
      adj: chunks.map(([p, k]) => chunkPaths.push(root + adjListChunk(e, a, p, k)) - 1),
      groups: e.property_groups.map((g) => chunks.map(([p, k]) => chunkPaths.push(root + edgePropertyChunk(e, a, g, p, k)) - 1)),
    };
  });
  const chunkNames = await names(chunkPaths);
  const files = (indices: readonly number[]) => list(indices.map((i) => chunkNames[i]!));

  // The internal vertex id is continuous: "each vertex assigned with a global index inside this
  // type (called internal vertex id) starting from 0, corresponding to the row number of the vertex
  // in the logical vertex table" (format.md, "Logical table of vertices"). The GraphAr paper allows
  // gaps ("bubbles"); the specification does not, and a global id that is a prefix sum of the
  // counts plus this index is injective only if there are none. So, per vertex type, the rows of
  // the first property group's chunks number `vertex_count` and the largest id is one below it. It
  // is a statement over the chunks' footers: Parquet keeps the row count and the extremes there.
  const checked = vertices.flatMap((v, i) => (vertexCounts[i] === 0 ? [] : [{ v, count: vertexCounts[i]!, group: vertexFiles[i]![0] }]))
    .filter((c) => c.group !== undefined);
  if (checked.length > 0) {
    const sql = checked
      .map(
        ({ v, group }) => `SELECT ${lit(v.type)} AS type, count(*) AS n, min(${ident(VERTEX_INDEX_COL)}) AS lo, max(${ident(VERTEX_INDEX_COL)}) AS hi
          FROM read_parquet(${files(group!)})`,
      )
      .join(' UNION ALL ');
    const answer = await run(sql, (cause) => new GraphArError('unreadable', 'the vertex chunks did not read', { cause }));
    const column = (name: string) => answer.getChild(name);
    for (let row = 0; row < answer.numRows; row++) {
      const type = String(column('type')!.get(row));
      const { count } = checked.find((c) => c.v.type === type)!;
      const [n, lo, hi] = ['n', 'lo', 'hi'].map((c) => Number(column(c)!.get(row)));
      if (n !== count || lo !== 0 || hi !== count - 1) {
        throw new GraphArError(
          'invalid',
          `vertex \`${type}\`: the internal vertex id (\`${VERTEX_INDEX_COL}\`) is not continuous: \`vertex_count\` is ${count}, ` +
            `the chunks hold ${n} rows with ids from ${lo} to ${hi}, and the specification numbers a type's vertices from 0 to ${count - 1}`,
          { document: type },
        );
      }
    }
  }

  const viewOf = (name: string, sql: string) => `CREATE OR REPLACE VIEW ${ident(catalog)}.${ident(name)} AS ${sql}`;
  const statements: string[] = [];
  vertices.forEach((v, i) => {
    const groups = vertexFiles[i]!;
    let sql: string;
    if (vertexCounts[i] === 0) {
      sql = relationOf([[VERTEX_INDEX_COL, 'BIGINT'], ...types(`vertex \`${v.type}\``, v.property_groups)], []);
    } else if (groups.length === 0) {
      sql = `SELECT CAST(range AS BIGINT) AS ${ident(VERTEX_INDEX_COL)} FROM range(${vertexCounts[i]})`;
    } else {
      // Every group's chunk carries the id, so the groups join on it.
      sql = `SELECT * FROM read_parquet(${files(groups[0]!)}) g0${groups
        .slice(1)
        .map((g, n) => ` JOIN read_parquet(${files(g)}) g${n + 1} USING (${ident(VERTEX_INDEX_COL)})`)
        .join('')}`;
    }
    statements.push(viewOf(v.type, sql));
  });
  edges.forEach((e, i) => {
    const { adj, groups } = edgeFiles[i]!;
    let sql: string;
    if (adj.length === 0) {
      sql = relationOf([[SRC_INDEX_COL, 'BIGINT'], [DST_INDEX_COL, 'BIGINT'], ...types(`edge \`${edgeName(e)}\``, e.property_groups)], []);
    } else if (groups.length === 0) {
      sql = `SELECT * FROM read_parquet(${files(adj)})`;
    } else {
      // A property chunk is row for row beside its adjacency-list chunk: joined on the chunk's
      // place in the list and the row's in the file, never on the order a scan happens to keep.
      const keyed = (indices: readonly number[]) =>
        `(SELECT * EXCLUDE (filename), list_position(${files(indices)}, filename) AS __graphar_chunk
          FROM read_parquet(${files(indices)}, filename = true, file_row_number = true))`;
      const using = 'USING (__graphar_chunk, file_row_number)';
      sql = `SELECT a.${ident(SRC_INDEX_COL)}, a.${ident(DST_INDEX_COL)}, ${groups
        .map((_, g) => `g${g}.* EXCLUDE (__graphar_chunk, file_row_number)`)
        .join(', ')} FROM ${keyed(adj)} a${groups.map((g, n) => ` JOIN ${keyed(g)} g${n} ${using}`).join('')}`;
    }
    statements.push(viewOf(edgeName(e), sql));
  });
  // One row per chunk file the views above read, by the name `resolve` gave it, and the offset
  // chunks of the adjacency list they read: what `parquet_metadata(path)` takes.
  const chunkRows: string[][] = [];
  const chunk = (type: string, kind: string, index: number) => chunkRows.push([lit(type), lit(kind), lit(chunkNames[index]!)]);
  vertices.forEach((v, i) => vertexFiles[i]!.forEach((g) => g.forEach((k) => chunk(v.type, 'vertex', k))));
  edges.forEach((e, i) => {
    const { offsets, adj, groups } = edgeFiles[i]!;
    const name = edgeName(e);
    offsets.forEach((k) => chunk(name, 'offset', k));
    adj.forEach((k) => chunk(name, 'adj_list', k));
    groups.forEach((g) => g.forEach((k) => chunk(name, 'edge_property', k)));
  });
  // Each property's counts over its chunks, from the row groups' own `Statistics`: nulls add up, the
  // bounds are the least and greatest in the column's type, and the distinct count of several row
  // groups lies between the largest one's and their sum (exact when there is one). A count some row
  // group does not carry is no count for the table. A list or a user-defined type is not counted:
  // Parquet's statistics are its leaf values', not the property's.
  const counted = [
    ...vertices.map((v, i) => [v.type, v.property_groups, vertexFiles[i]!] as const),
    ...edges.map((e, i) => [edgeName(e), e.property_groups, edgeFiles[i]!.groups] as const),
  ].flatMap(([type, groups, perGroup]) =>
    groups.flatMap((g, n) =>
      g.properties
        .filter((p) => p.cardinality === 'single' && Object.hasOwn(GRAPHAR_TYPES, p.data_type) && perGroup[n]!.length > 0)
        .map((p) => {
          const t = GRAPHAR_TYPES[p.data_type]!;
          const all = (c: string) => `count(*) = count(${c})`;
          return `SELECT ${lit(type)} AS type, ${lit(p.name)} AS property,
            CASE WHEN ${all('stats_null_count')} THEN sum(stats_null_count) END AS null_count,
            CAST(min(TRY_CAST(stats_min_value AS ${t})) AS VARCHAR) AS min,
            CAST(max(TRY_CAST(stats_max_value AS ${t})) AS VARCHAR) AS max,
            CASE WHEN ${all('stats_distinct_count')} THEN max(stats_distinct_count) END AS distinct_min,
            CASE WHEN ${all('stats_distinct_count')} THEN sum(stats_distinct_count) END AS distinct_max
            FROM parquet_metadata(${files(perGroup[n]!)}) WHERE path_in_schema = ${lit(p.name)}`;
        }),
    ),
  );
  const STATS: readonly (readonly [string, string])[] = [
    ['type', 'VARCHAR'], ['property', 'VARCHAR'], ['null_count', 'UBIGINT'], ['min', 'VARCHAR'], ['max', 'VARCHAR'],
    ['distinct_min', 'UBIGINT'], ['distinct_max', 'UBIGINT'],
  ];
  statements.push(
    viewOf('graphar_column_stats', counted.length === 0
      ? relationOf(STATS, [])
      : `SELECT ${STATS.map(([c, t]) => `CAST(${ident(c)} AS ${t}) AS ${ident(c)}`).join(', ')} FROM (${counted.join(' UNION ALL ')})`),
    viewOf('graphar_chunks', relationOf([['type', 'VARCHAR'], ['kind', 'VARCHAR'], ['path', 'VARCHAR']], chunkRows)),
    viewOf('graphar_vertex_types', relationOf(
      [['type', 'VARCHAR'], ['count', 'UBIGINT'], ['ordinal', 'INTEGER']],
      vertices.map((v, i) => [lit(v.type), String(vertexCounts[i]), String(i + 1)]),
    )),
    viewOf('graphar_edge_types', relationOf(
      [['name', 'VARCHAR'], ['src_type', 'VARCHAR'], ['edge_type', 'VARCHAR'], ['dst_type', 'VARCHAR'], ['count', 'UBIGINT'],
        ['directed', 'BOOLEAN'], ['ordinal', 'INTEGER']],
      edges.map((e, i) => [
        lit(edgeName(e)), lit(e.src_type), lit(e.edge_type), lit(e.dst_type),
        String(partCounts[i]!.reduce((a, b) => a + b, 0)), String(e.directed), String(i + 1),
      ]),
    )),
    viewOf('graphar_properties', relationOf(
      [['type', 'VARCHAR'], ['property', 'VARCHAR'], ['data_type', 'VARCHAR'], ['is_primary', 'BOOLEAN'], ['is_nullable', 'BOOLEAN'],
        ['ordinal', 'INTEGER'], ['cardinality', 'VARCHAR']],
      [
        ...vertices.map((v) => [v.type, v.property_groups] as const),
        ...edges.map((e) => [edgeName(e), e.property_groups] as const),
      ].flatMap(([type, groups]) =>
        groups.flatMap((g) => g.properties).map((p, n) => [
          lit(type), lit(p.name), lit(p.data_type),
          String(p.is_primary), String(p.is_nullable), String(n + 1), lit(p.cardinality),
        ]),
      ),
    )),
    viewOf('graphar_extra_info', relationOf(
      [['key', 'VARCHAR'], ['value', 'VARCHAR']],
      graph.extra_info.map(([k, v]) => [lit(k), lit(v)]),
    )),
  );

  // The catalog, shared by every attachment of its name on this engine.
  const held = holders.get(engine) ?? new Map<string, number>();
  holders.set(engine, held);
  held.set(catalog, (held.get(catalog) ?? 0) + 1);
  const release = async (): Promise<void> => {
    const left = (held.get(catalog) ?? 1) - 1;
    if (left > 0) return void held.set(catalog, left);
    held.delete(catalog);
    await run(`DETACH DATABASE IF EXISTS ${ident(catalog)}`, (cause) => new GraphArError('engine', `detach ${catalog}`, { cause }), undefined);
  };
  try {
    await run(`ATTACH IF NOT EXISTS ':memory:' AS ${ident(catalog)}`, (cause) => new GraphArError('engine', `attach ${catalog}`, { cause }));
    for (const statement of statements) {
      await run(statement, (cause) => new GraphArError('engine', `the engine refused: ${statement.slice(0, 200)}`, { cause }));
    }
  } catch (cause) {
    await release().catch(() => {});
    throw cause;
  }
  let detached = false;
  const detach = async (): Promise<void> => {
    if (detached) return;
    detached = true;
    await release();
  };

  // What the consumer reads: each column's DuckDB type as the engine typed the view, and each
  // property's statistics from the relation above.
  let columns: Table;
  let stats: Table;
  try {
    columns = await run(
      `SELECT table_name AS r, column_name AS c, data_type AS t FROM information_schema.columns WHERE table_catalog = ${lit(catalog)}`,
      (cause) => new GraphArError('engine', `the columns of ${catalog} did not read`, { cause }),
    );
    stats = await run(`SELECT * FROM ${ident(catalog)}.graphar_column_stats`, (cause) =>
      new GraphArError('engine', `the statistics of ${catalog} did not read`, { cause }));
  } catch (cause) {
    await detach().catch(() => {});
    throw cause;
  }
  const cell = (t: Table, row: number, c: string) => t.getChild(c)?.get(row);
  const typeOf = new Map<string, string>();
  for (let row = 0; row < columns.numRows; row++) {
    typeOf.set(`${cell(columns, row, 'r')}\0${cell(columns, row, 'c')}`, String(cell(columns, row, 't')));
  }
  const statsOf = new Map<string, Stats>();
  for (let row = 0; row < stats.numRows; row++) {
    const count = (c: string) => {
      const v = cell(stats, row, c);
      return v === null || v === undefined ? undefined : Number(v);
    };
    const text = (c: string) => {
      const v = cell(stats, row, c);
      return v === null || v === undefined ? undefined : String(v);
    };
    const [lo, hi] = [count('distinct_min'), count('distinct_max')];
    statsOf.set(`${cell(stats, row, 'type')}\0${cell(stats, row, 'property')}`, {
      ...(count('null_count') === undefined ? {} : { nulls: count('null_count')! }),
      ...(text('min') === undefined ? {} : { min: text('min')! }),
      ...(text('max') === undefined ? {} : { max: text('max')! }),
      ...(lo === undefined || hi === undefined ? {} : { distinct: lo === hi ? { exact: lo } : { atLeast: lo, atMost: hi } }),
    });
  }
  const propertiesOf = (relation: string, groups: readonly PropertyGroup[]): GraphProperty[] =>
    groups.flatMap((g) => g.properties).map((p) => {
      const s = statsOf.get(`${relation}\0${p.name}`);
      return {
        name: p.name,
        type: typeOf.get(`${relation}\0${p.name}`) ?? propertyType(p, `\`${relation}\``, userTypes),
        primary: p.is_primary,
        nullable: p.is_nullable,
        list: p.cardinality !== 'single',
        ...(s === undefined ? {} : { stats: s }),
      };
    });
  const table = (name: string) => `${ident(catalog)}.${ident(name)}`;
  return graphOf({
    engine,
    vertexTypes: vertices.map((v, i) => ({
      type: v.type, count: vertexCounts[i]!, table: table(v.type), index: VERTEX_INDEX_COL,
      properties: propertiesOf(v.type, v.property_groups),
    })),
    edgeTypes: edges.map((e, i) => ({
      name: edgeName(e), edgeType: e.edge_type, srcType: e.src_type, dstType: e.dst_type,
      count: partCounts[i]!.reduce((a, b) => a + b, 0), directed: e.directed,
      table: table(edgeName(e)), src: SRC_INDEX_COL, dst: DST_INDEX_COL,
      properties: propertiesOf(edgeName(e), e.property_groups),
    })),
    detach,
  });
}
