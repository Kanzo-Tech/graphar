/**
 * The door: a GraphAr graph attached to the host's DuckDB as a catalog — DuckDB's `ATTACH … AS
 * name` — and the handle that detaches it.
 */

import { GraphArError } from './error.js';
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
}

/** A graph attached by {@link attachGraph}. */
export interface GraphAttachment {
  /** The catalog: `"<catalog>"."person"`. */
  readonly catalog: string;
  /** The information files, parsed. */
  readonly graph: GraphInfo;
  /**
   * Detaches the catalog, once no other attachment of the same name on the engine holds it. A
   * second call does nothing.
   */
  detach(): Promise<void>;
  /** {@link GraphAttachment.detach}, for `await using`. */
  [Symbol.asyncDispose](): Promise<void>;
}

/** GraphAr's reserved columns: C++ `GeneralParams` (`general_params.h`). */
export const VERTEX_INDEX_COL = '_graphArVertexIndex';
export const SRC_INDEX_COL = '_graphArSrcIndex';
export const DST_INDEX_COL = '_graphArDstIndex';

/** The relations the graph's information files become, beside the views. */
const METADATA = ['graphar_vertex_types', 'graphar_edge_types', 'graphar_properties', 'graphar_extra_info', 'graphar_chunks'] as const;

const holders = new WeakMap<Engine, Map<string, number>>();

/** A single-quoted SQL string literal. */
const lit = (value: string): string => `'${value.replace(/'/g, "''")}'`;
/** A quoted SQL identifier. */
const ident = (name: string): string => `"${name.replace(/"/g, '""')}"`;
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

/** GraphAr's type as DuckDB's, for a relation with no file to read its type from. */
function duckType(dataType: string, at: string): string {
  const list = /^list<(.+)>$/.exec(dataType);
  if (list) return `${duckType(list[1]!, at)}[]`;
  const t = ({
    bool: 'BOOLEAN', int32: 'INTEGER', int64: 'BIGINT', float: 'FLOAT', double: 'DOUBLE', string: 'VARCHAR',
    date: 'DATE', timestamp: 'TIMESTAMP', time: 'TIME',
  } as Record<string, string>)[dataType];
  if (t === undefined) throw new GraphArError('unsupported', `${at}: data type \`${dataType}\` has no DuckDB type here`);
  return t;
}

const propertyType = (p: Property, at: string): string =>
  p.cardinality === 'single' ? duckType(p.data_type, at) : `${duckType(p.data_type, at)}[]`;

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

/**
 * **Attach a GraphAr graph to the engine as the catalog `catalog`**: a view per vertex type,
 * `"<catalog>"."<type>"`, and per edge type, `"<catalog>"."<src_type>_<edge_type>_<dst_type>"`,
 * with GraphAr's own columns, and the information files as five relations.
 *
 * ```ts
 * await using g = await attachGraph(engine, 'job', (paths) => paths.map((p) => `${url}/${p}`), { entry: 'fossil.graph.yml' });
 * await engine.query(`SELECT _graphArVertexIndex, name FROM "job"."person"`, { signal });
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
 * - `graphar_properties(type, property, data_type, is_primary, is_nullable, ordinal)`: one row per
 *   property; `type` is the vertex type or the edge type's `name`; `data_type` is GraphAr's word;
 *   `ordinal` is the column's place in the type's view after GraphAr's id columns, from 1.
 * - `graphar_extra_info(key, value)`: the graph document's `extra_info`, in order.
 * - `graphar_chunks(type, kind, path)`: one row per Parquet chunk file the views read, in the order
 *   vertex type, then edge type; `kind` is `vertex`, `offset`, `adj_list` or `edge_property`; `path` is
 *   exactly the name `resolve` gave the file, so `parquet_metadata(path)` and
 *   `parquet_kv_metadata(path)` read it. Only the edge type's first adjacency list is listed, with
 *   its offset chunks if it is ordered.
 *
 * A graph `prefix` is read against the graph document's directory, where GraphAr's C++ library
 * reads a relative one against its working directory; without one, the data is under the document,
 * which is GraphAr's default. Only Parquet is read.
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
): Promise<GraphAttachment> {
  const { entry, signal } = options;
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
  const types = (at: string, groups: readonly PropertyGroup[]) =>
    groups.flatMap((g) => g.properties.map((p) => [p.name, propertyType(p, at)] as const));
  for (const v of vertices) {
    for (const g of v.property_groups) parquet(`vertex \`${v.type}\``, g.file_type);
    types(`vertex \`${v.type}\``, v.property_groups);
  }
  for (const e of edges) {
    parquet(`edge \`${edgeName(e)}\`, ${e.adj_lists[0]!.type}`, e.adj_lists[0]!.file_type);
    for (const g of e.property_groups) parquet(`edge \`${edgeName(e)}\``, g.file_type);
    types(`edge \`${edgeName(e)}\``, e.property_groups);
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

  const viewOf = (name: string, sql: string) => `CREATE OR REPLACE VIEW ${ident(catalog)}.${ident(name)} AS ${sql}`;
  const statements: string[] = [];
  vertices.forEach((v, i) => {
    const groups = vertexFiles[i]!;
    let sql: string;
    if (vertexCounts[i] === 0) {
      sql = relationOf([[VERTEX_INDEX_COL, 'BIGINT'], ...types('', v.property_groups)], []);
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
      sql = relationOf([[SRC_INDEX_COL, 'BIGINT'], [DST_INDEX_COL, 'BIGINT'], ...types('', e.property_groups)], []);
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
  statements.push(
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
        ['ordinal', 'INTEGER']],
      [
        ...vertices.map((v) => [v.type, v.property_groups] as const),
        ...edges.map((e) => [edgeName(e), e.property_groups] as const),
      ].flatMap(([type, groups]) =>
        groups.flatMap((g) => g.properties).map((p, n) => [
          lit(type), lit(p.name), lit(p.cardinality === 'single' ? p.data_type : `${p.cardinality}<${p.data_type}>`),
          String(p.is_primary), String(p.is_nullable), String(n + 1),
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
  return { catalog, graph, detach, [Symbol.asyncDispose]: detach };
}
