/**
 * The graph, as a consumer reads it: its types and their properties, the reads a view over it
 * makes, and patterns across types translated to SQL — with no name of the format in sight. Every
 * column a consumer needs it is handed by an object here; it never writes one.
 */

import { GraphArError } from './error.js';
import type { Engine, Table } from './attach.js';

/** What every read that can be stopped takes. */
export interface Signal {
  readonly signal?: AbortSignal;
}

/**
 * An attached Apache GraphAr graph. A vertex is its type and its index within the type, from 0 —
 * GraphAr's internal vertex id — and an edge gives the index of each end within its type. There is
 * no id across types: the specification defines none.
 */
export interface Graph {
  /** In the order of the graph document's `vertices`. */
  readonly vertexTypes: readonly VertexType[];
  /** In the order of the graph document's `edges`. */
  readonly edgeTypes: readonly EdgeType[];
  /** The properties of one vertex, by name, or `null` when the type has no vertex of that index. */
  vertex(type: string, index: number, options?: Signal): Promise<Readonly<Record<string, unknown>> | null>;
  /** Every edge of an edge type, by {@link EdgeType.name}: the index of each end, in the order stored. */
  links(edgeType: string, options?: Signal): Promise<{ readonly src: Uint32Array; readonly dst: Uint32Array }>;
  /**
   * The edges at one vertex, each with the vertex at its other end, ordered by edge type as the graph
   * lists them, `out` before `in`, then the other end's index: `out` where it is the source,
   * `in` where it is the destination, `both` (the default) for either. An edge of an undirected
   * type is read as stored, so it is `out` at the end GraphAr keeps as its source.
   */
  neighbours(type: string, index: number, options?: Signal & { readonly direction?: 'out' | 'in' | 'both' }): Promise<Neighbour[]>;
  /**
   * The vertices with a string property that contains `text`, case aside: at most `limit` (20 by
   * default), in the order type, index, property.
   */
  search(text: string, options?: Signal & { readonly limit?: number }): Promise<Match[]>;
  /**
   * The most common values of a property of a vertex or an edge type, as text, each with how many
   * times it occurs; the members of a list, for a list. At most `limit` (10 by default), the most
   * common first. Read with one query when asked: nothing keeps them.
   */
  values(type: string, property: string, options?: Signal & { readonly limit?: number }): Promise<Value[]>;
  /**
   * A fixed-length pattern across types — vertex, edge, vertex, … — as one SQL query, the way
   * SQL/PGQ's `GRAPH_TABLE (… MATCH … COLUMNS …)` reads: a row per match, a column per entry of
   * `columns`, each `variable.property` and named so (`"u.name"`), or a vertex's bare variable for
   * its index within its type, named by the variable (`"u"`), which is what a {@link Graph.reach}
   * predicate over that type is applied to. Without `columns`, every property of every named
   * element. A `where` is SQL over the element's own columns, by their bare names.
   */
  match(pattern: Pattern, options?: { readonly columns?: readonly string[] }): { readonly sql: string; readonly columns: readonly Column[] };
  /**
   * The vertices of `to` that some vertex of `from.type` satisfying `from.where` reaches, as a
   * predicate over `to`'s {@link VertexType.table}: what a filter on one type is on another. The
   * path is the one shortest path between the two types; when there are several, `via` names it,
   * a {@link Pattern} from `from.type` to `to`. The same type is the filter itself.
   */
  reach(from: { readonly type: string; readonly where: string }, to: string, options?: { readonly via?: Pattern }): string;
  /** Detaches the graph, once nothing else on the engine holds it. A second call does nothing. */
  detach(): Promise<void>;
  /** {@link Graph.detach}, for `await using`. */
  [Symbol.asyncDispose](): Promise<void>;
}

/** A vertex type. */
export interface VertexType {
  readonly type: string;
  readonly count: number;
  /** The relation to query, quoted for a `FROM`: the index, then every property. */
  readonly table: string;
  /** The column of the index within the type, unquoted. */
  readonly index: string;
  /** In the order of the type's property groups. */
  readonly properties: readonly Property[];
}

/** An edge type: GraphAr's triple of source type, edge type and destination type. */
export interface EdgeType {
  /** What names the type in a read: GraphAr's `<srcType>_<edgeType>_<dstType>`. */
  readonly name: string;
  readonly edgeType: string;
  readonly srcType: string;
  readonly dstType: string;
  readonly count: number;
  readonly directed: boolean;
  /** The relation to query, quoted for a `FROM`: the two ends, then every property. */
  readonly table: string;
  /** The column of the source's index within {@link EdgeType.srcType}, unquoted. */
  readonly src: string;
  /** The column of the destination's index within {@link EdgeType.dstType}, unquoted. */
  readonly dst: string;
  readonly properties: readonly Property[];
}

/** A property: a column of its type's {@link VertexType.table} of the same name. */
export interface Property {
  readonly name: string;
  /** The column's DuckDB type: `VARCHAR`, `BIGINT[]`. */
  readonly type: string;
  readonly primary: boolean;
  readonly nullable: boolean;
  /** A GraphAr `list` or `set`: the column is a DuckDB list. */
  readonly list: boolean;
  /** Absent for a list, a user-defined type, and a type with no rows. */
  readonly stats?: Stats;
}

/**
 * What the chunks' Parquet `Statistics` say of a property, never read from a value. A count some
 * chunk does not carry is absent.
 */
export interface Stats {
  readonly nulls?: number;
  /** The least value, as text. */
  readonly min?: string;
  /** The greatest value, as text. */
  readonly max?: string;
  /**
   * How many distinct values: exact when the chunks say so, otherwise between the largest chunk's
   * count and their sum.
   */
  readonly distinct?: { readonly exact: number } | { readonly atLeast: number; readonly atMost: number };
}

/** An edge at a vertex, from {@link Graph.neighbours}. */
export interface Neighbour {
  /** The edge type's {@link EdgeType.name}. */
  readonly edgeType: string;
  readonly direction: 'out' | 'in';
  /** The vertex at the other end: its type and index. */
  readonly type: string;
  readonly index: number;
  /** The edge's properties, by name. */
  readonly properties: Readonly<Record<string, unknown>>;
}

/** A vertex {@link Graph.search} found, and the value that matched. */
export interface Match {
  readonly type: string;
  readonly index: number;
  readonly property: string;
  readonly value: string;
}

/** A value of a property and how often it occurs, from {@link Graph.values}. */
export interface Value {
  readonly value: string;
  readonly count: number;
}

/** A vertex in a {@link Pattern}. */
export interface VertexPattern {
  /** The variable a column or a `where` names it by. */
  readonly as?: string;
  readonly type: string;
  /** SQL over the type's columns, by their bare names. */
  readonly where?: string;
}

/** An edge in a {@link Pattern}, between the vertices before and after it. */
export interface EdgePattern {
  readonly as?: string;
  /** GraphAr's `edge_type`; the vertices beside it say which source and destination. */
  readonly edge: string;
  /** `out` (the default): from the vertex before to the vertex after; `in`: the other way. */
  readonly direction?: 'out' | 'in';
  readonly where?: string;
}

/** Vertex, edge, vertex, …: a fixed path, as SQL/PGQ's `MATCH (p:Person)-[:knows]->(q:Person)`. */
export type Pattern = readonly (VertexPattern | EdgePattern)[];

/** A column of {@link Graph.match}'s answer. */
export interface Column {
  /** Its name in the answer: `u.name`, or `u` for a vertex's index. */
  readonly name: string;
  readonly variable: string;
  /** The property it reads; absent for a vertex's index within its type. */
  readonly property?: Property;
}

/** A single-quoted SQL string literal. */
const lit = (value: string): string => `'${value.replace(/'/g, "''")}'`;
/** A quoted SQL identifier. */
const ident = (name: string): string => `"${name.replace(/"/g, '""')}"`;

/** What {@link graphOf} is built from: the attachment's types, the engine and the detach. */
export interface Parts {
  readonly engine: Engine;
  readonly vertexTypes: readonly VertexType[];
  readonly edgeTypes: readonly EdgeType[];
  readonly detach: () => Promise<void>;
}

/** The reserved prefix of a variable {@link Graph.match} names itself. */
const HIDDEN = '__graphar_';

type Element =
  | { readonly kind: 'vertex'; readonly alias: string; readonly named: boolean; readonly of: VertexType; readonly where?: string | undefined }
  | { readonly kind: 'edge'; readonly alias: string; readonly named: boolean; readonly of: EdgeType; readonly near: string; readonly far: string; readonly where?: string | undefined };

/** The {@link Graph} over an attached graph's views. */
export function graphOf({ engine, vertexTypes, edgeTypes, detach }: Parts): Graph {
  const quiet = new AbortController().signal;
  const query = async (sql: string, signal: AbortSignal | undefined): Promise<Table> => {
    signal?.throwIfAborted();
    try {
      return await engine.query(sql, { signal: signal ?? quiet });
    } catch (cause) {
      signal?.throwIfAborted();
      throw new GraphArError('engine', `the engine refused: ${sql.slice(0, 200)}`, { cause });
    }
  };
  const vertexType = (type: string): VertexType => {
    const v = vertexTypes.find((t) => t.type === type);
    if (!v) throw new GraphArError('unknown', `the graph has no vertex type \`${type}\``);
    return v;
  };
  const edgeType = (name: string): EdgeType => {
    const e = edgeTypes.find((t) => t.name === name);
    if (!e) throw new GraphArError('unknown', `the graph has no edge type \`${name}\``);
    return e;
  };
  const record = (answer: Table, row: number, properties: readonly Property[]): Record<string, unknown> =>
    Object.fromEntries(properties.map((p) => [p.name, answer.getChild(p.name)?.get(row) ?? null]));
  const limitOf = (limit: number | undefined, fallback: number): number => {
    const n = limit ?? fallback;
    if (!Number.isSafeInteger(n) || n < 0) throw new RangeError(`limit ${n} is not a count`);
    return n;
  };

  /** The pattern as joins: each element's relation and alias, and the join that reaches it. */
  const elements = (pattern: Pattern): Element[] => {
    if (pattern.length % 2 === 0) throw new TypeError('a pattern is vertex, edge, vertex, …: an odd number of elements');
    const aliases = new Set<string>();
    const aliasOf = (as: string | undefined, i: number): string => {
      if (as === undefined) return `${HIDDEN}${i}`;
      if (as.startsWith(HIDDEN)) throw new TypeError(`\`${as}\`: a variable does not start with \`${HIDDEN}\``);
      if (aliases.has(as)) throw new TypeError(`\`${as}\` names two elements of the pattern`);
      aliases.add(as);
      return as;
    };
    return pattern.map((element, i): Element => {
      if (i % 2 === 0) {
        if (!('type' in element)) throw new TypeError(`element ${i} of a pattern is a vertex: it has a \`type\``);
        return { kind: 'vertex', alias: aliasOf(element.as, i), named: element.as !== undefined, of: vertexType(element.type), where: element.where };
      }
      if (!('edge' in element)) throw new TypeError(`element ${i} of a pattern is an edge: it has an \`edge\``);
      const before = (pattern[i - 1] as VertexPattern).type;
      const after = (pattern[i + 1] as VertexPattern).type;
      const out = (element.direction ?? 'out') === 'out';
      const [srcType, dstType] = out ? [before, after] : [after, before];
      const of = edgeTypes.find((e) => e.srcType === srcType && e.edgeType === element.edge && e.dstType === dstType);
      if (!of) throw new GraphArError('unknown', `the graph has no edge type \`${element.edge}\` from \`${srcType}\` to \`${dstType}\``);
      return {
        kind: 'edge', alias: aliasOf(element.as, i), named: element.as !== undefined, of, where: element.where,
        near: out ? of.src : of.dst, far: out ? of.dst : of.src,
      };
    });
  };

  /** `FROM … JOIN …` over the elements, each filtered by its own `where` before it joins. */
  const from = (chain: readonly Element[]): string =>
    chain
      .map((el, i) => {
        const relation = el.where === undefined ? el.of.table : `(SELECT * FROM ${el.of.table} WHERE (${el.where}))`;
        const at = `${relation} AS ${ident(el.alias)}`;
        if (i === 0) return `FROM ${at}`;
        const prev = chain[i - 1]!;
        const on =
          el.kind === 'edge'
            ? `${ident(el.alias)}.${ident(el.near)} = ${ident(prev.alias)}.${ident((prev.of as VertexType).index)}`
            : `${ident(el.alias)}.${ident(el.of.index)} = ${ident(prev.alias)}.${ident((prev as Extract<Element, { kind: 'edge' }>).far)}`;
        return `JOIN ${at} ON ${on}`;
      })
      .join(' ');

  /** The one shortest path between two vertex types, as a pattern, or why there is none. */
  const shortest = (source: string, target: string): Pattern => {
    type Step = { readonly to: string; readonly edge: EdgeType; readonly direction: 'out' | 'in' };
    const steps = (type: string): Step[] =>
      edgeTypes.flatMap((e) => [
        ...(e.srcType === type ? [{ to: e.dstType, edge: e, direction: 'out' as const }] : []),
        ...(e.dstType === type ? [{ to: e.srcType, edge: e, direction: 'in' as const }] : []),
      ]);
    // Breadth first, counting the shortest paths into each type; one predecessor is kept, which is
    // the path whenever the count is 1.
    const paths = new Map<string, number>([[source, 1]]);
    const back = new Map<string, { readonly from: string; readonly step: Step }>();
    let frontier = [source];
    while (frontier.length > 0 && !paths.has(target)) {
      const next = new Map<string, number>();
      for (const type of frontier) {
        for (const step of steps(type)) {
          if (paths.has(step.to)) continue;
          next.set(step.to, (next.get(step.to) ?? 0) + paths.get(type)!);
          if (!back.has(step.to)) back.set(step.to, { from: type, step });
        }
      }
      for (const [type, n] of next) paths.set(type, n);
      frontier = [...next.keys()];
    }
    const n = paths.get(target);
    if (n === undefined) throw new GraphArError('unknown', `no path joins \`${source}\` to \`${target}\``);
    if (n > 1) throw new GraphArError('ambiguous', `${n} shortest paths join \`${source}\` to \`${target}\`: \`via\` names one`);
    const pattern: (VertexPattern | EdgePattern)[] = [{ type: target }];
    for (let at = target; at !== source; ) {
      const { from: prev, step } = back.get(at)!;
      pattern.unshift({ type: prev }, { edge: step.edge.edgeType, direction: step.direction });
      at = prev;
    }
    return pattern;
  };

  const graph: Graph = {
    vertexTypes,
    edgeTypes,

    async vertex(type, index, options = {}) {
      const v = vertexType(type);
      if (!Number.isSafeInteger(index) || index < 0 || index >= v.count) return null;
      const answer = await query(`SELECT * FROM ${v.table} WHERE ${ident(v.index)} = ${index}`, options.signal);
      return answer.numRows === 0 ? null : record(answer, 0, v.properties);
    },

    async links(name, options = {}) {
      const e = edgeType(name);
      const answer = await query(
        `SELECT CAST(${ident(e.src)} AS UINTEGER) AS s, CAST(${ident(e.dst)} AS UINTEGER) AS d FROM ${e.table}`,
        options.signal,
      );
      const src = new Uint32Array(answer.numRows);
      const dst = new Uint32Array(answer.numRows);
      const s = answer.getChild('s')!;
      const d = answer.getChild('d')!;
      for (let i = 0; i < answer.numRows; i++) {
        src[i] = Number(s.get(i));
        dst[i] = Number(d.get(i));
      }
      return { src, dst };
    },

    async neighbours(type, index, options = {}) {
      const v = vertexType(type);
      const direction = options.direction ?? 'both';
      const ends = edgeTypes.flatMap((e) => [
        ...(direction !== 'in' && e.srcType === v.type ? [{ e, at: e.src, other: e.dst, type: e.dstType, direction: 'out' as const }] : []),
        ...(direction !== 'out' && e.dstType === v.type ? [{ e, at: e.dst, other: e.src, type: e.srcType, direction: 'in' as const }] : []),
      ]);
      const answers = await Promise.all(
        ends.map(({ e, at, other }) =>
          query(
            `SELECT CAST(${ident(other)} AS UINTEGER) AS ${ident(`${HIDDEN}other`)}, * FROM ${e.table} WHERE ${ident(at)} = ${index} ORDER BY 1`,
            options.signal,
          ),
        ),
      );
      return ends.flatMap(({ e, type: otherType, direction: d }, n) => {
        const answer = answers[n]!;
        const other = answer.getChild(`${HIDDEN}other`)!;
        return Array.from({ length: answer.numRows }, (_, row) => ({
          edgeType: e.name, direction: d, type: otherType, index: Number(other.get(row)), properties: record(answer, row, e.properties),
        }));
      });
    },

    async search(text, options = {}) {
      const limit = limitOf(options.limit, 20);
      if (text === '' || limit === 0) return [];
      const selects = vertexTypes.flatMap((v, t) =>
        v.properties
          .filter((p) => p.type === 'VARCHAR')
          .map(
            (p) => `SELECT ${t} AS t, CAST(${ident(v.index)} AS UINTEGER) AS i, ${lit(p.name)} AS p, ${ident(p.name)} AS v
              FROM ${v.table} WHERE contains(lower(${ident(p.name)}), lower(${lit(text)}))`,
          ),
      );
      if (selects.length === 0) return [];
      const answer = await query(`${selects.join(' UNION ALL ')} ORDER BY t, i, p LIMIT ${limit}`, options.signal);
      const [t, i, p, v] = ['t', 'i', 'p', 'v'].map((c) => answer.getChild(c)!);
      return Array.from({ length: answer.numRows }, (_, row) => ({
        type: vertexTypes[Number(t!.get(row))]!.type, index: Number(i!.get(row)), property: String(p!.get(row)), value: String(v!.get(row)),
      }));
    },

    async values(type, property, options = {}) {
      const limit = limitOf(options.limit, 10);
      const of = vertexTypes.find((t) => t.type === type) ?? edgeTypes.find((t) => t.name === type);
      if (!of) throw new GraphArError('unknown', `the graph has no vertex or edge type \`${type}\``);
      const p = of.properties.find((q) => q.name === property);
      if (!p) throw new GraphArError('unknown', `\`${type}\` has no property \`${property}\``);
      const column = ident(p.name);
      const rows = p.list ? `(SELECT unnest(${column}) AS ${column} FROM ${of.table})` : of.table;
      const answer = await query(
        `SELECT CAST(${column} AS VARCHAR) AS v, CAST(count(*) AS DOUBLE) AS n FROM ${rows}
          WHERE ${column} IS NOT NULL GROUP BY 1 ORDER BY n DESC, v LIMIT ${limit}`,
        options.signal,
      );
      const [v, n] = ['v', 'n'].map((c) => answer.getChild(c)!);
      return Array.from({ length: answer.numRows }, (_, row) => ({ value: String(v!.get(row)), count: Number(n!.get(row)) }));
    },

    match(pattern, options = {}) {
      const chain = elements(pattern);
      const named = chain.filter((el) => el.named);
      const columns: Column[] = (options.columns ?? named.flatMap((el) => el.of.properties.map((p) => `${el.alias}.${p.name}`))).map((name) => {
        const dot = name.indexOf('.');
        const variable = dot < 0 ? name : name.slice(0, dot);
        const el = named.find((x) => x.alias === variable);
        if (!el || (dot < 0 && el.kind !== 'vertex')) {
          throw new GraphArError('unknown', `\`${name}\` is not \`variable.property\`, nor a vertex variable, of the pattern`);
        }
        if (dot < 0) return { name, variable };
        const property = el.of.properties.find((p) => p.name === name.slice(dot + 1));
        if (!property) throw new GraphArError('unknown', `\`${variable}\` has no property \`${name.slice(dot + 1)}\``);
        return { name, variable, property };
      });
      const index = (variable: string) => (named.find((x) => x.alias === variable)!.of as VertexType).index;
      const list = columns.map(
        (c) => `${ident(c.variable)}.${ident(c.property?.name ?? index(c.variable))} AS ${ident(c.name)}`,
      );
      return { sql: `SELECT ${list.length === 0 ? 'count(*) AS n' : list.join(', ')} ${from(chain)}`, columns };
    },

    reach(source, to, options = {}) {
      const target = vertexType(to);
      vertexType(source.type);
      const path = options.via ?? (source.type === to ? undefined : shortest(source.type, to));
      if (path === undefined) return `(${source.where})`;
      const first = path[0] as VertexPattern | undefined;
      const last = path[path.length - 1] as VertexPattern | undefined;
      if (first?.type !== source.type || last?.type !== to) {
        throw new TypeError(`\`via\` runs from \`${source.type}\` to \`${to}\`: its first vertex is one and its last the other`);
      }
      const where = first.where === undefined ? source.where : `(${first.where}) AND (${source.where})`;
      const chain = elements([{ ...first, where }, ...path.slice(1)]);
      // The last vertex joins only to be filtered: its index is the last edge's far end.
      const end = chain[chain.length - 1]!;
      const reached =
        end.where === undefined && chain.length > 1
          ? (() => {
              const edge = chain[chain.length - 2] as Extract<Element, { kind: 'edge' }>;
              return { chain: chain.slice(0, -1), column: `${ident(edge.alias)}.${ident(edge.far)}` };
            })()
          : { chain, column: `${ident(end.alias)}.${ident(target.index)}` };
      return `${ident(target.index)} IN (SELECT ${reached.column} ${from(reached.chain)})`;
    },

    detach,
    [Symbol.asyncDispose]: detach,
  };
  return graph;
}
