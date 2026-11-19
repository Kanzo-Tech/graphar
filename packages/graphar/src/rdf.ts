/**
 * A graph read as RDF, with what its mapping says: how each table and column becomes triples.
 * Labels are not the mapping's but the shapes', and are read from the shape document saved beside
 * the graph.
 */

import type { Literal, NamedNode, Quad, Quad_Object, Quad_Subject } from '@rdfjs/types';
import { DataFactory, Parser } from 'n3';

import { type Engine, type Reader, ident, lit } from './attach.js';
import { GraphArError } from './error.js';
import type { EdgeType, Graph, Property, Signal, VertexType } from './graph.js';

/** The RML-Core mapping `graphar-rdf` saves beside the information files. */
export const MAPPING_FILE = 'mapping.ttl';
/** The graph document's `extra_info` key naming the RML-Core mapping saved beside the graph. */
export const MAPPING_KEY = 'mapping';
/** The graph document's `extra_info` key naming the shape document saved beside the graph. */
export const SHAPES_KEY = 'shapes';

/** The syntax of each shape document `graphar-rdf` saves, by its file name. */
const SHAPE_FILES = { 'shapes.ttl': 'shacl', 'shapes.shex': 'shexc', 'shapes.shexj': 'shexj' } as const;

/**
 * A graph read as RDF: its types and properties with what the mapping says of them, and its
 * triples. {@link Graph.rdf}, for a graph whose document names a mapping.
 */
export interface Rdf {
  /** The graph's vertex types, in its order, each with its class, shape and labels. */
  readonly vertexTypes: readonly RdfVertexType[];
  /** The graph's edge types, in its order, each with its predicate. */
  readonly edgeTypes: readonly RdfEdgeType[];
  /**
   * The triples that match `pattern`, as one SQL query over the graph's views: a row per triple,
   * with the columns {@link TRIPLE_COLUMNS}, every one `VARCHAR`. Only the triples maps that can
   * make such a triple are read, and a bound subject or object is a filter on its column: the
   * mapping read backwards. Without a pattern, every triple. The query composes with any other
   * SQL on the engine.
   */
  match(pattern?: TriplePattern): { readonly sql: string };
  /**
   * The triples that match `pattern`, {@link Rdf.match} run on the engine, as terms. A literal
   * reads in its canonical lexical form: the writer stored its value.
   */
  triples(pattern?: TriplePattern, options?: Signal): AsyncIterable<Quad>;
  /** The RML-Core mapping, as it was saved. */
  readonly mapping: string;
  /** The shape document the graph conforms to, as it was saved; absent when none was. */
  readonly shapes?: Shapes;
}

/** A triple pattern: each position bound to a term, or free when absent. */
export interface TriplePattern {
  readonly subject?: NamedNode;
  readonly predicate?: NamedNode;
  readonly object?: NamedNode | Literal;
}

/**
 * The columns of {@link Rdf.match}: the subject's IRI, the predicate's, the object's IRI or
 * lexical form, the object's term type (`IRI` or `Literal`), a literal's datatype, and a
 * language-tagged string's tag.
 */
export const TRIPLE_COLUMNS = ['subject', 'predicate', 'object', 'term_type', 'datatype', 'language'] as const;

/** A shape document, as the writer saved it. */
export interface Shapes {
  /** SHACL in Turtle, ShEx in ShExC, or ShEx in ShExJ. */
  readonly syntax: 'shacl' | 'shexc' | 'shexj';
  readonly text: string;
}

/** What the shapes say of a type or a property: absent where they say nothing. */
export interface Labelled {
  /**
   * SHACL's `sh:name`, ShExJ's `rdfs:label` annotation: the one with no language tag, else the
   * `en` one, else the first by language tag.
   */
  readonly label?: string;
  /** SHACL's `sh:description`, ShExJ's `rdfs:comment` annotation, chosen as {@link Labelled.label} is. */
  readonly description?: string;
}

/** A vertex type: the subjects of one shape. */
export interface RdfVertexType extends VertexType, Labelled {
  /** The class the type's shape fixes: every subject has this `rdf:type`. */
  readonly iri?: string;
  /** The IRI of the shape every subject conforms to; absent when no shape typed the graph. */
  readonly shape?: string;
  readonly properties: readonly RdfProperty[];
}

/** An edge type: a predicate from the subjects of one vertex type to those of another. */
export interface RdfEdgeType extends EdgeType, Labelled {
  /** The predicate. */
  readonly iri: string;
}

/** A property: the subject's IRI, or the objects of one predicate. */
export interface RdfProperty extends Property, Labelled {
  /** The predicate; absent for the subject's own column. */
  readonly iri?: string;
  readonly termType: 'IRI' | 'Literal';
  /**
   * A literal's datatype: `rdf:langString` for a language-tagged string, whose tag each value
   * carries (the column's `lang` field).
   */
  readonly datatype?: string;
}

const RML = 'http://w3id.org/rml/';
const RDF = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';
const RDF_TYPE = `${RDF}type`;
const RDF_LANG_STRING = `${RDF}langString`;
const XSD_STRING = 'http://www.w3.org/2001/XMLSchema#string';
const DCT_CONFORMS_TO = 'http://purl.org/dc/terms/conformsTo';
const SH = 'http://www.w3.org/ns/shacl#';
const RDFS_LABEL = 'http://www.w3.org/2000/01/rdf-schema#label';
const RDFS_COMMENT = 'http://www.w3.org/2000/01/rdf-schema#comment';
/** What the triples maps' relative names resolve against: a name nothing dereferences. */
const BASE = 'https://graphar.invalid/mapping.ttl';

/** An object map and its predicate: what it reads and the term it makes. */
interface ObjectMap {
  readonly predicate: string;
  /** The column, as the mapping writes it: a plain or a delimited SQL identifier. */
  readonly reference: string;
  readonly termType: 'IRI' | 'Literal';
  readonly datatype?: string;
  /** The column of the language tag. */
  readonly language?: string;
}

/** A triples map, in the subset `graphar-rdf` writes. */
interface TriplesMap {
  /** The GraphAr names its IRI's fragment holds: `[type]`, `[type, column]` or `[edge]`. */
  readonly name: readonly string[];
  /** The relation, as SQL: a table name or a query. */
  readonly source: { readonly table: string } | { readonly query: string };
  readonly subject: string;
  readonly classes: readonly string[];
  readonly shape?: string;
  readonly objects: readonly ObjectMap[];
}

type Node = Quad_Subject | Quad_Object;

const unsupported = (what: string) => new GraphArError('unsupported', `${MAPPING_FILE}: ${what}`, { document: MAPPING_FILE });
const invalid = (what: string) => new GraphArError('invalid', `${MAPPING_FILE}: ${what}`, { document: MAPPING_FILE });

/** A plain or delimited SQL identifier, as RML-IO and R2RML read a column or a table name. */
const IDENTIFIER = /^(?:[A-Za-z_][A-Za-z0-9_]*|"(?:[^"]|"")+")$/;
/** The name an identifier names: a delimited one undelimited. */
const named = (identifier: string): string =>
  identifier.startsWith('"') ? identifier.slice(1, -1).replace(/""/g, '"') : identifier;

/**
 * The triples maps of `text`. Every triple of the document is a part of one, or it is refused:
 * only the RML-Core subset `graphar-rdf` writes is read, and anything else is `unsupported`.
 */
function triplesMaps(text: string): TriplesMap[] {
  let quads: Quad[];
  try {
    quads = new Parser({ baseIRI: BASE }).parse(text);
  } catch (cause) {
    throw new GraphArError('unparseable', `${MAPPING_FILE}: not Turtle`, { document: MAPPING_FILE, cause });
  }
  const read = new Set<Quad>();
  /** The predicates of `node`, each triple read once; any other predicate is refused. */
  const fields = (node: Node, at: string, allowed: readonly string[]): Map<string, Quad_Object[]> => {
    const out = new Map<string, Quad_Object[]>();
    for (const q of quads.filter((q) => q.subject.equals(node))) {
      const p = q.predicate.value;
      if (!allowed.includes(p)) throw unsupported(`${at}: <${p}> is not a part of the RML-Core graphar-rdf writes`);
      read.add(q);
      out.set(p, [...(out.get(p) ?? []), q.object]);
    }
    return out;
  };
  const optional = (values: Map<string, Quad_Object[]>, p: string, at: string): Quad_Object | undefined => {
    const v = values.get(p) ?? [];
    if (v.length > 1) throw invalid(`${at}: more than one <${p}>`);
    return v[0];
  };
  const one = (values: Map<string, Quad_Object[]>, p: string, at: string): Quad_Object => {
    const v = optional(values, p, at);
    if (v === undefined) throw invalid(`${at}: no <${p}>`);
    return v;
  };
  const literal = (term: Quad_Object, at: string): string => {
    if (term.termType !== 'Literal') throw invalid(`${at}: not a literal`);
    return term.value;
  };
  const iri = (term: Quad_Object, at: string): string => {
    if (term.termType !== 'NamedNode') throw invalid(`${at}: not an IRI`);
    return term.value;
  };
  const reference = (term: Quad_Object, at: string): string => {
    const r = literal(term, at);
    if (!IDENTIFIER.test(r)) throw unsupported(`${at}: the reference \`${r}\` is not a column name`);
    return r;
  };
  const termType = (term: Quad_Object | undefined, at: string): 'IRI' | 'Literal' | undefined => {
    if (term === undefined) return undefined;
    const t = iri(term, at);
    if (t === `${RML}IRI`) return 'IRI';
    if (t === `${RML}Literal`) return 'Literal';
    throw unsupported(`${at}: the term type <${t}>`);
  };

  const maps = quads.filter((q) => q.predicate.value === RDF_TYPE && q.object.value === `${RML}TriplesMap`).map((q) => q.subject);
  const out = maps.map((m): TriplesMap => {
    const at = `<${m.value}>`;
    if (m.termType !== 'NamedNode' || !m.value.startsWith(`${BASE}#`)) throw invalid(`${at}: a triples map not named in the document`);
    const name = m.value.slice(BASE.length + 1).split('/').map(decodeURIComponent);
    const map = fields(m, at, [RDF_TYPE, `${RML}logicalSource`, `${RML}subjectMap`, `${RML}predicateObjectMap`, DCT_CONFORMS_TO]);
    if ((map.get(RDF_TYPE) ?? []).some((t) => t.value !== `${RML}TriplesMap`)) throw unsupported(`${at}: a type other than rml:TriplesMap`);

    const source = fields(one(map, `${RML}logicalSource`, at), `${at} logical source`, [`${RML}source`, `${RML}referenceFormulation`, `${RML}iterator`]);
    const described = fields(one(source, `${RML}source`, at), `${at} source`, [RDF_TYPE]);
    if ((described.get(RDF_TYPE) ?? []).map((t) => t.value).join() !== `${RML}Source`) throw unsupported(`${at}: a source other than an anonymous rml:Source`);
    const formulation = iri(one(source, `${RML}referenceFormulation`, at), `${at} reference formulation`);
    const iterator = literal(one(source, `${RML}iterator`, at), `${at} iterator`);
    let relation: TriplesMap['source'];
    if (formulation === `${RML}SQL2008Table`) {
      if (!IDENTIFIER.test(iterator)) throw unsupported(`${at}: the table \`${iterator}\` is not a table name`);
      relation = { table: iterator };
    } else if (formulation === `${RML}SQL2008Query`) {
      relation = { query: iterator };
    } else {
      throw unsupported(`${at}: the reference formulation <${formulation}>`);
    }

    const subject = fields(one(map, `${RML}subjectMap`, at), `${at} subject map`, [`${RML}reference`, `${RML}termType`, `${RML}class`]);
    if (termType(optional(subject, `${RML}termType`, at), `${at} subject map`) === 'Literal') throw invalid(`${at}: a literal subject`);
    const objects = (map.get(`${RML}predicateObjectMap`) ?? []).map((pom): ObjectMap => {
      const p = fields(pom, `${at} predicate-object map`, [`${RML}predicate`, `${RML}objectMap`]);
      const predicate = iri(one(p, `${RML}predicate`, at), `${at} predicate`);
      const where = `${at} object map of <${predicate}>`;
      const om = fields(one(p, `${RML}objectMap`, at), where, [`${RML}reference`, `${RML}termType`, `${RML}datatype`, `${RML}languageMap`]);
      const datatype = optional(om, `${RML}datatype`, where);
      const languageMap = optional(om, `${RML}languageMap`, where);
      // RML-Core: an object map with a reference makes a literal unless its term type says otherwise.
      const type = termType(optional(om, `${RML}termType`, where), where) ?? 'Literal';
      if (type === 'IRI' && (datatype !== undefined || languageMap !== undefined)) throw invalid(`${where}: an IRI with a datatype or a language`);
      if (datatype !== undefined && languageMap !== undefined) throw invalid(`${where}: a datatype and a language`);
      const language =
        languageMap === undefined ? undefined : reference(one(fields(languageMap, `${where} language map`, [`${RML}reference`]), `${RML}reference`, where), where);
      return {
        predicate,
        reference: reference(one(om, `${RML}reference`, where), where),
        termType: type,
        ...(datatype === undefined ? {} : { datatype: iri(datatype, `${where} datatype`) }),
        ...(language === undefined ? {} : { language }),
      };
    });
    const shape = optional(map, DCT_CONFORMS_TO, at);
    return {
      name,
      source: relation,
      subject: reference(one(subject, `${RML}reference`, at), `${at} subject map`),
      classes: (subject.get(`${RML}class`) ?? []).map((c) => iri(c, `${at} class`)),
      ...(shape === undefined ? {} : { shape: iri(shape, `${at} shape`) }),
      objects,
    };
  });
  const left = quads.find((q) => !read.has(q));
  if (left !== undefined) {
    throw unsupported(`<${left.subject.value}> <${left.predicate.value}>: a triple outside the triples maps graphar-rdf writes`);
  }
  return out;
}

/** Of literals as given, the untagged one, else the `en` one, else the first by tag. */
function chosen(values: readonly { readonly value: string; readonly language: string }[]): string | undefined {
  const sorted = [...values].sort((a, b) => (a.language < b.language ? -1 : a.language > b.language ? 1 : 0));
  return (sorted.find((v) => v.language === '') ?? sorted.find((v) => v.language === 'en') ?? sorted[0])?.value;
}

const labelled = (label: string | undefined, description: string | undefined): Labelled => ({
  ...(label === undefined ? {} : { label }),
  ...(description === undefined ? {} : { description }),
});

/** What the shapes say: of a shape, and of a property of it, by its predicate. */
interface Labels {
  shape(shape: string): Labelled;
  property(shape: string, predicate: string): Labelled;
}

const NONE: Labels = { shape: () => ({}), property: () => ({}) };

/** SHACL: `sh:name` and `sh:description` of the node shape and of its property shapes by `sh:path`. */
function shaclLabels(text: string, document: string): Labels {
  let quads: Quad[];
  try {
    quads = new Parser().parse(text);
  } catch (cause) {
    throw new GraphArError('unparseable', `${document}: not Turtle`, { document, cause });
  }
  const objects = (s: Node, p: string) => quads.filter((q) => q.subject.equals(s) && q.predicate.value === p).map((q) => q.object);
  const words = (s: Node, p: string) =>
    chosen(objects(s, p).flatMap((o) => (o.termType === 'Literal' ? [{ value: o.value, language: o.language }] : [])));
  const of = (s: Node) => labelled(words(s, `${SH}name`), words(s, `${SH}description`));
  const node = (shape: string): NamedNode => DataFactory.namedNode(shape);
  return {
    shape: (shape) => of(node(shape)),
    property: (shape, predicate) => {
      const ps = objects(node(shape), `${SH}property`).find((p) =>
        objects(p, `${SH}path`).some((path) => path.termType === 'NamedNode' && path.value === predicate),
      );
      return ps === undefined ? {} : of(ps);
    },
  };
}

/** ShExJ: the `rdfs:label` and `rdfs:comment` annotations of a shape and of its triple constraints. */
function shexjLabels(text: string, document: string): Labels {
  type Json = Record<string, unknown>;
  let schema: unknown;
  try {
    schema = JSON.parse(text);
  } catch (cause) {
    throw new GraphArError('unparseable', `${document}: not JSON`, { document, cause });
  }
  const isObject = (v: unknown): v is Json => typeof v === 'object' && v !== null && !Array.isArray(v);
  const array = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
  const of = (annotations: unknown): Labelled => {
    const words = (predicate: string) =>
      chosen(
        array(annotations).flatMap((a) => {
          if (!isObject(a) || a['predicate'] !== predicate) return [];
          const o = a['object'];
          if (!isObject(o) || typeof o['value'] !== 'string') return [];
          return [{ value: o['value'], language: typeof o['language'] === 'string' ? o['language'] : '' }];
        }),
      );
    return labelled(words(RDFS_LABEL), words(RDFS_COMMENT));
  };
  const shapes = new Map<string, Json>();
  for (const decl of array(isObject(schema) ? schema['shapes'] : undefined)) {
    if (!isObject(decl) || typeof decl['id'] !== 'string') continue;
    // ShEx 2.1 declares a shape in a `ShapeDecl`; ShEx 2.0 gave the shape expression the id.
    const expr = decl['type'] === 'ShapeDecl' ? decl['shapeExpr'] : decl;
    if (isObject(expr) && expr['type'] === 'Shape') shapes.set(decl['id'], expr);
  }
  const constraints = (e: unknown): Json[] => {
    if (!isObject(e)) return [];
    if (e['type'] === 'TripleConstraint') return [e];
    if (e['type'] === 'EachOf' || e['type'] === 'OneOf') return array(e['expressions']).flatMap(constraints);
    return [];
  };
  return {
    shape: (shape) => of(shapes.get(shape)?.['annotations']),
    property: (shape, predicate) =>
      of(constraints(shapes.get(shape)?.['expression']).find((tc) => tc['predicate'] === predicate && tc['inverse'] !== true)?.['annotations']),
  };
}

/**
 * **The RDF reading of an attached graph** whose document names a mapping under {@link MAPPING_KEY}:
 * each type and property with what the mapping says of it and the labels its shape document gives,
 * and its triples.
 *
 * The mapping is read beside the graph document, through the engine like every other file, and
 * parsed as Turtle; only the RML-Core subset `graphar-rdf` writes is read. A triples map is named by
 * the GraphAr names it reads — `<#Person>`, `<#Person/nick>`, `<#Person_knows_Person>` — so each is
 * matched to its type, column or edge type by its name, never by reading its SQL.
 *
 * The shape document is the one `extra_info` names under {@link SHAPES_KEY}: `shapes.ttl` (SHACL:
 * `sh:name`, `sh:description`), `shapes.shexj` (ShExJ: `rdfs:label`, `rdfs:comment` annotations) or
 * `shapes.shex` (ShExC, whose annotations are not read: there is no ShExC parser here). Without
 * shapes there are no labels: a consumer shows an IRI's local name.
 *
 * {@link Rdf.triples} runs the SQL the mapping holds, so a graph is as trusted as the code it would
 * run: the host's engine decides what a statement may reach.
 *
 * @throws {GraphArError} `unreadable` for a mapping or shape document that does not read;
 *   `unparseable` for one that does not parse; `unsupported` for a mapping outside the subset
 *   `graphar-rdf` writes; `invalid` for one that does not map the graph's types, or an `extra_info`
 *   naming a file `graphar-rdf` does not write.
 */
export async function rdfOf(
  engine: Engine,
  graph: Graph,
  files: Reader,
  base: string,
  extra: readonly (readonly [string, string])[],
): Promise<Rdf | undefined> {
  const named = (key: string) => extra.find(([k]) => k === key)?.[1];
  const file = named(MAPPING_KEY);
  if (file === undefined) return undefined;
  if (file !== MAPPING_FILE) {
    throw new GraphArError('invalid', `extra_info \`${MAPPING_KEY}\` names \`${file}\`, not the \`${MAPPING_FILE}\` graphar-rdf writes`);
  }
  const [mapping] = (await files.texts([base + MAPPING_FILE])) as [string];
  const maps = triplesMaps(mapping);
  let shapes: Shapes | undefined;
  let labels = NONE;
  const shapeFile = named(SHAPES_KEY);
  if (shapeFile !== undefined) {
    const syntax = Object.hasOwn(SHAPE_FILES, shapeFile) ? SHAPE_FILES[shapeFile as keyof typeof SHAPE_FILES] : undefined;
    if (syntax === undefined) {
      throw new GraphArError('invalid', `extra_info \`${SHAPES_KEY}\` names \`${shapeFile}\`, no shape document graphar-rdf writes`);
    }
    const [text] = (await files.texts([base + shapeFile])) as [string];
    shapes = { syntax, text };
    labels = syntax === 'shacl' ? shaclLabels(text, base + shapeFile) : syntax === 'shexj' ? shexjLabels(text, base + shapeFile) : NONE;
  }
  return rdf(engine, graph, maps, labels, mapping, shapes);
}

/** A literal's datatype as {@link TRIPLE_COLUMNS} gives it: a tagged string's is `rdf:langString`. */
const datatypeOf = (o: ObjectMap): string | undefined =>
  o.termType === 'IRI' ? undefined : o.language !== undefined ? RDF_LANG_STRING : (o.datatype ?? XSD_STRING);

/** The {@link Rdf} of an attached graph and its triples maps. */
function rdf(engine: Engine, graph: Graph, maps: readonly TriplesMap[], labels: Labels, mapping: string, shapes: Shapes | undefined): Rdf {
  const key = (name: readonly string[]) => name.join('/');
  const byName = new Map(maps.map((m) => [key(m.name), m]));
  for (const m of maps) {
    const [first, column] = m.name;
    const known =
      (m.name.length === 1 && (graph.vertexTypes.some((v) => v.type === first) || graph.edgeTypes.some((e) => e.name === first))) ||
      (m.name.length === 2 && graph.vertexTypes.find((v) => v.type === first)?.properties.some((p) => p.name === column) === true);
    if (!known) throw invalid(`the triples map <#${key(m.name)}> names no type, column or edge type of the graph`);
  }

  const vertexTypes = graph.vertexTypes.map((v): RdfVertexType => {
    const own = byName.get(v.type);
    if (own !== undefined && !('table' in own.source && named(own.source.table) === v.type)) {
      throw invalid(`the triples map <#${v.type}> reads a relation other than \`${v.type}\``);
    }
    if (own !== undefined && own.classes.length > 1) throw unsupported(`<#${v.type}>: more than one class`);
    const shape = own?.shape;
    const subjects = new Set(maps.filter((m) => m.name[0] === v.type && (m.name.length === 2 || m === own)).map((m) => named(m.subject)));
    const properties = v.properties.map((p): RdfProperty => {
      const column = byName.get(key([v.type, p.name]));
      if (column !== undefined && column.objects.length !== 1) throw unsupported(`<#${v.type}/${p.name}>: more than one predicate-object map`);
      const object = column === undefined ? own?.objects.find((o) => named(o.reference) === p.name) : column.objects[0];
      if (object === undefined) {
        if (subjects.has(p.name)) return { ...p, termType: 'IRI' };
        throw invalid(`no triples map reads \`${v.type}\`.\`${p.name}\``);
      }
      const datatype = datatypeOf(object);
      return {
        ...p,
        iri: object.predicate,
        termType: object.termType,
        ...(datatype === undefined ? {} : { datatype }),
        ...(shape === undefined ? {} : labels.property(shape, object.predicate)),
      };
    });
    const iri = own?.classes[0];
    return { ...v, ...(iri === undefined ? {} : { iri }), ...(shape === undefined ? {} : { shape, ...labels.shape(shape) }), properties };
  });
  const edgeTypes = graph.edgeTypes.map((e): RdfEdgeType => {
    const m = byName.get(e.name);
    const object = m?.objects.length === 1 ? m.objects[0] : undefined;
    if (object === undefined) throw invalid(`no triples map with one predicate reads the edge type \`${e.name}\``);
    const shape = vertexTypes.find((v) => v.type === e.srcType)?.shape;
    return { ...e, iri: object.predicate, ...(shape === undefined ? {} : labels.property(shape, object.predicate)) };
  });

  // The mapping's logical tables are the graph's relations by name (`rml:source` is an anonymous
  // `rml:Source`: the host binds the tables). The query binds them to the catalog's views as its
  // own common table expressions, which a table name in it resolves to before any table: nothing
  // is set on the engine, and the mapping's SQL is run as it is written.
  const relations = [...graph.vertexTypes.map((v) => [v.type, v.table] as const), ...graph.edgeTypes.map((e) => [e.name, e.table] as const)];
  const bound = relations.length === 0 ? '' : `WITH ${relations.map(([name, table]) => `${ident(name)} AS (SELECT * FROM ${table})`).join(', ')} `;
  const text = (sql: string) => `CAST(${sql} AS VARCHAR)`;
  const row = (subject: string, predicate: string, object: string, termType: string, datatype: string, language: string) =>
    `SELECT ${[subject, predicate, object, termType, datatype, language].map((c, i) => `${c} AS ${TRIPLE_COLUMNS[i]}`).join(', ')}`;
  const NULL = 'CAST(NULL AS VARCHAR)';

  function match(pattern: TriplePattern = {}): { readonly sql: string } {
    const { subject, predicate, object } = pattern;
    const selects: string[] = [];
    for (const m of maps) {
      const from = 'table' in m.source ? m.source.table : `(${m.source.query})`;
      const s = text(`q.${m.subject}`);
      const where = [`q.${m.subject} IS NOT NULL`, ...(subject === undefined ? [] : [`${s} = ${lit(subject.value)}`])];
      // A class is a constant object: the pattern decides at once whether the map makes it.
      if (predicate === undefined || predicate.value === RDF_TYPE) {
        for (const c of m.classes) {
          if (object !== undefined && !(object.termType === 'NamedNode' && object.value === c)) continue;
          selects.push(`${row(s, lit(RDF_TYPE), lit(c), lit('IRI'), NULL, NULL)} FROM ${from} AS q WHERE ${where.join(' AND ')}`);
        }
      }
      for (const o of m.objects) {
        if (predicate !== undefined && predicate.value !== o.predicate) continue;
        const datatype = datatypeOf(o);
        const value = text(`q.${o.reference}`);
        const tag = o.language === undefined ? NULL : text(`q.${o.language}`);
        const filters = [...where, `q.${o.reference} IS NOT NULL`, ...(o.language === undefined ? [] : [`q.${o.language} IS NOT NULL`])];
        if (object !== undefined) {
          if (object.termType === 'NamedNode') {
            if (o.termType !== 'IRI') continue;
          } else {
            if (o.termType !== 'Literal' || object.datatype.value !== datatype) continue;
            if (o.language !== undefined) filters.push(`lower(${tag}) = ${lit(object.language.toLowerCase())}`);
          }
          filters.push(`${value} = ${lit(object.value)}`);
        }
        selects.push(
          `${row(s, lit(o.predicate), value, lit(o.termType), datatype === undefined ? NULL : lit(datatype), tag)} FROM ${from} AS q WHERE ${filters.join(' AND ')}`,
        );
      }
    }
    const body = selects.length === 0 ? `${row(NULL, NULL, NULL, NULL, NULL, NULL)} WHERE false` : selects.join(' UNION ALL ');
    return { sql: `${bound}${body}` };
  }

  const quiet = new AbortController().signal;
  async function* triples(pattern: TriplePattern = {}, options: Signal = {}): AsyncIterable<Quad> {
    const { signal } = options;
    const { sql } = match(pattern);
    signal?.throwIfAborted();
    let answer;
    try {
      answer = await engine.query(sql, { signal: signal ?? quiet });
    } catch (cause) {
      signal?.throwIfAborted();
      throw new GraphArError('engine', `the triples did not read: ${sql.slice(0, 200)}`, { cause });
    }
    const [s, p, o, t, d, l] = TRIPLE_COLUMNS.map((c) => answer.getChild(c));
    const { namedNode, literal, quad } = DataFactory;
    const nodes = new Map<string, NamedNode>();
    const node = (iri: string) => nodes.get(iri) ?? (nodes.set(iri, namedNode(iri)), nodes.get(iri)!);
    for (let i = 0; i < answer.numRows; i++) {
      const value = String(o!.get(i));
      const datatype = d!.get(i);
      const language = l!.get(i);
      const term =
        t!.get(i) === 'IRI'
          ? node(value)
          : language !== null && language !== undefined
            ? literal(value, String(language))
            : datatype === XSD_STRING
              ? literal(value)
              : literal(value, node(String(datatype)));
      yield quad(node(String(s!.get(i))), node(String(p!.get(i))), term);
    }
  }
  return { vertexTypes, edgeTypes, match, triples, mapping, ...(shapes === undefined ? {} : { shapes }) };
}
