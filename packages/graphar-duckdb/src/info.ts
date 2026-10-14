/**
 * Apache GraphAr's information files — the graph, vertex and edge YAML documents — as plain data,
 * in GraphAr's own key names, with the defaults GraphAr's C++ library applies (`graph_info.cc`
 * v0.13.0). It is the TypeScript reading of the Rust crate `graphar-info`, and refuses what that
 * crate refuses: a key GraphAr does not define, a version other than `gar/v1`.
 */

import { load } from 'js-yaml';

import { GraphArError } from './error.js';

/** A graph: the `<name>.graph.yml` document, with the vertex and edge documents it names. */
export interface GraphInfo {
  readonly name: string;
  /** The root of the data files. Absent is GraphAr's default: the graph document's directory. */
  readonly prefix?: string;
  /** In the order the document lists them. */
  readonly vertices: readonly VertexInfo[];
  /** In the order the document lists them. */
  readonly edges: readonly EdgeInfo[];
  readonly labels: readonly string[];
  readonly version: string;
  /** User-defined `key`/`value` pairs, in order, never interpreted. */
  readonly extra_info: readonly (readonly [key: string, value: string])[];
}

/** A vertex type: one `<type>.vertex.yml` document. */
export interface VertexInfo {
  readonly type: string;
  readonly chunk_size: number;
  /** Under the graph's root; `<type>/` when the document names none. */
  readonly prefix: string;
  readonly labels: readonly string[];
  readonly property_groups: readonly PropertyGroup[];
  readonly version: string;
}

/** An edge type: one `<src_type>_<edge_type>_<dst_type>.edge.yml` document. */
export interface EdgeInfo {
  readonly src_type: string;
  readonly edge_type: string;
  readonly dst_type: string;
  readonly chunk_size: number;
  readonly src_chunk_size: number;
  readonly dst_chunk_size: number;
  /** `false` when the document says nothing. */
  readonly directed: boolean;
  /** Under the graph's root; `<src_type>_<edge_type>_<dst_type>/` when the document names none. */
  readonly prefix: string;
  readonly adj_lists: readonly AdjacentList[];
  readonly property_groups: readonly PropertyGroup[];
  readonly version: string;
}

/** `ordered_by_source`, `ordered_by_dest`, `unordered_by_source` or `unordered_by_dest`. */
export type AdjListType = `${'ordered' | 'unordered'}_by_${'source' | 'dest'}`;

/** One stored adjacency list of an edge type. */
export interface AdjacentList {
  readonly type: AdjListType;
  readonly file_type: string;
  /** Under the edge type's prefix; `<type>/` when the document names none. */
  readonly prefix: string;
}

/** Properties stored together. */
export interface PropertyGroup {
  readonly properties: readonly Property[];
  readonly file_type: string;
  /** Under the type's prefix; the property names joined by `_`, and a slash, when none is named. */
  readonly prefix: string;
}

/** One property: a column of a vertex or edge type. */
export interface Property {
  readonly name: string;
  /** GraphAr's word for it: `int64`, `string`, `list<float>`, … */
  readonly data_type: string;
  readonly is_primary: boolean;
  /** `!is_primary` when the document says nothing. */
  readonly is_nullable: boolean;
  /** `single`, `list` or `set`; `single` when the document says nothing. */
  readonly cardinality: string;
}

/** The `<src_type>_<edge_type>_<dst_type>` that names an edge type's file and table. */
export const edgeName = (e: Pick<EdgeInfo, 'src_type' | 'edge_type' | 'dst_type'>): string =>
  `${e.src_type}_${e.edge_type}_${e.dst_type}`;

type Doc = Record<string, unknown>;

/** A document's keys, every one of them GraphAr's, read as `T` by `read`. */
class Reader {
  constructor(
    private readonly doc: Doc,
    private readonly at: string,
  ) {}

  static of(value: unknown, at: string, keys: readonly string[]): Reader {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) throw invalid(at, 'is not a mapping');
    for (const key of Object.keys(value)) {
      if (!keys.includes(key)) throw invalid(at, `\`${key}\` is not a key GraphAr defines`);
    }
    return new Reader(value as Doc, at);
  }

  string(key: string): string;
  string(key: string, fallback: string): string;
  string(key: string, fallback?: string): string {
    const v = this.doc[key];
    if (v === undefined || v === null) {
      if (fallback === undefined) throw invalid(this.at, `\`${key}\` is missing`);
      return fallback;
    }
    if (typeof v !== 'string') throw invalid(this.at, `\`${key}\` is not a string`);
    return v;
  }

  optionalString(key: string): string | undefined {
    return this.doc[key] === undefined || this.doc[key] === null ? undefined : this.string(key);
  }

  count(key: string): number {
    const v = this.doc[key];
    if (typeof v !== 'number' || !Number.isSafeInteger(v) || v <= 0) {
      throw invalid(this.at, `\`${key}\` is not a positive integer`);
    }
    return v;
  }

  bool(key: string, fallback: boolean): boolean {
    const v = this.doc[key];
    if (v === undefined || v === null) return fallback;
    if (typeof v !== 'boolean') throw invalid(this.at, `\`${key}\` is not a boolean`);
    return v;
  }

  list(key: string): unknown[] {
    const v = this.doc[key];
    if (v === undefined || v === null) return [];
    if (!Array.isArray(v)) throw invalid(this.at, `\`${key}\` is not a list`);
    return v;
  }

  strings(key: string): string[] {
    return this.list(key).map((v) => {
      if (typeof v !== 'string') throw invalid(this.at, `\`${key}\` holds a value that is not a string`);
      return v;
    });
  }

  version(): string {
    const v = this.string('version');
    if (!/^gar\/v1(\s*\([^)]*\))?$/.test(v.trim())) throw invalid(this.at, `version \`${v}\`: GraphAr defines \`gar/v1\``);
    return v;
  }
}

const invalid = (document: string, reason: string): GraphArError =>
  new GraphArError('invalid', `${document}: ${reason}`, { document });

/** The YAML text as a value, or `unparseable`. */
function yaml(text: string, document: string): unknown {
  try {
    return load(text);
  } catch (cause) {
    throw new GraphArError('unparseable', `${document}: ${(cause as Error).message}`, { document, cause });
  }
}

function group(value: unknown, at: string): PropertyGroup {
  const g = Reader.of(value, at, ['prefix', 'file_type', 'properties']);
  const properties = g.list('properties').map((p, i): Property => {
    const r = Reader.of(p, `${at}, property ${i}`, ['name', 'data_type', 'is_primary', 'is_nullable', 'cardinality']);
    const is_primary = r.bool('is_primary', false);
    return {
      name: r.string('name'),
      data_type: r.string('data_type'),
      is_primary,
      is_nullable: r.bool('is_nullable', !is_primary),
      cardinality: r.string('cardinality', 'single'),
    };
  });
  if (properties.length === 0) throw invalid(at, '`properties` is empty');
  return {
    properties,
    file_type: g.string('file_type'),
    prefix: g.optionalString('prefix') ?? `${properties.map((p) => p.name).join('_')}/`,
  };
}

/** Parses one vertex document. */
export function parseVertex(text: string, document: string): VertexInfo {
  const r = Reader.of(yaml(text, document), document, ['type', 'chunk_size', 'prefix', 'labels', 'property_groups', 'version']);
  const type = r.string('type');
  return {
    type,
    chunk_size: r.count('chunk_size'),
    prefix: r.optionalString('prefix') ?? `${type}/`,
    labels: r.strings('labels'),
    property_groups: r.list('property_groups').map((g, i) => group(g, `${document}, property group ${i}`)),
    version: r.version(),
  };
}

/** Parses one edge document. */
export function parseEdge(text: string, document: string): EdgeInfo {
  const r = Reader.of(yaml(text, document), document, [
    'src_type', 'edge_type', 'dst_type', 'chunk_size', 'src_chunk_size', 'dst_chunk_size', 'prefix', 'directed',
    'adj_lists', 'property_groups', 'version',
  ]);
  const ends = { src_type: r.string('src_type'), edge_type: r.string('edge_type'), dst_type: r.string('dst_type') };
  const adj_lists = r.list('adj_lists').map((a, i): AdjacentList => {
    const at = `${document}, adjacency list ${i}`;
    const l = Reader.of(a, at, ['ordered', 'aligned_by', 'prefix', 'file_type']);
    const by = l.string('aligned_by');
    if (by !== 'src' && by !== 'dst') throw invalid(at, `\`aligned_by\` is \`${by}\`, not \`src\` or \`dst\``);
    const type: AdjListType = `${l.bool('ordered', false) ? 'ordered' : 'unordered'}_by_${by === 'src' ? 'source' : 'dest'}`;
    return { type, file_type: l.string('file_type'), prefix: l.optionalString('prefix') ?? `${type}/` };
  });
  if (adj_lists.length === 0) throw invalid(document, '`adj_lists` is empty');
  return {
    ...ends,
    chunk_size: r.count('chunk_size'),
    src_chunk_size: r.count('src_chunk_size'),
    dst_chunk_size: r.count('dst_chunk_size'),
    directed: r.bool('directed', false),
    prefix: r.optionalString('prefix') ?? `${edgeName(ends)}/`,
    adj_lists,
    property_groups: r.list('property_groups').map((g, i) => group(g, `${document}, property group ${i}`)),
    version: r.version(),
  };
}

/** The graph document, before the documents it names are read. */
export interface GraphDocument {
  readonly info: Omit<GraphInfo, 'vertices' | 'edges'>;
  /** The vertex documents' paths, as the graph document writes them. */
  readonly vertices: readonly string[];
  /** The edge documents' paths, as the graph document writes them. */
  readonly edges: readonly string[];
}

/** Parses the graph document. */
export function parseGraph(text: string, document: string): GraphDocument {
  const r = Reader.of(yaml(text, document), document, ['name', 'prefix', 'vertices', 'edges', 'labels', 'version', 'extra_info']);
  const extra_info = r.list('extra_info').map((kv, i) => {
    const e = Reader.of(kv, `${document}, extra_info ${i}`, ['key', 'value']);
    return [e.string('key'), e.string('value')] as const;
  });
  const prefix = r.optionalString('prefix');
  return {
    info: {
      name: r.string('name'),
      ...(prefix === undefined ? {} : { prefix }),
      labels: r.strings('labels'),
      version: r.version(),
      extra_info,
    },
    vertices: r.strings('vertices'),
    edges: r.strings('edges'),
  };
}
