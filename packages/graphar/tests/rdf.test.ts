/**
 * `conformance/rdf`, the graphs `graphar-rdf`'s example writes from
 * `crates/graphar-rdf/tests/data/people.ttl` and each of its three shape documents, attached to a
 * native DuckDB and read back as RDF.
 */

import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve as resolvePath } from 'node:path';

import type { Quad, Term } from '@rdfjs/types';
import { DataFactory, Parser } from 'n3';
import { afterAll, describe, expect, it } from 'vitest';

import { attach, GraphArError, type Rdf, type Resolve, type TriplePattern } from '../src/index.js';
import { engine } from './engine.js';

const RDF = resolvePath(import.meta.dirname, '../../../conformance/rdf');
const DATA = resolvePath(import.meta.dirname, '../../../crates/graphar-rdf/tests/data');
const SAMPLE = resolvePath(import.meta.dirname, '../../../conformance/sample');
const under = (dir: string): Resolve => (paths) => paths.map((p) => `${dir}/${p}`);
const entry = 'graph.graph.yml';
const XSD = 'http://www.w3.org/2001/XMLSchema#';

const temporary: string[] = [];
afterAll(() => Promise.all(temporary.map((d) => rm(d, { recursive: true, force: true }))));

/** A copy of a graph, with `edit` applied to one of its files, or the file removed. */
async function copy(from: string, file: string, edit: ((text: string) => string) | null): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), 'graphar-rdf-'));
  temporary.push(dir);
  await cp(from, dir, { recursive: true });
  if (edit === null) await rm(join(dir, file));
  else await writeFile(join(dir, file), edit(await readFile(join(dir, file), 'utf8')));
  return dir;
}

const term = (t: Term): string => {
  if (t.termType === 'NamedNode') return `<${t.value}>`;
  if (t.termType === 'Literal') {
    const tail = t.language !== '' ? `@${t.language}` : t.datatype.value === `${XSD}string` ? '' : `^^<${t.datatype.value}>`;
    return `${JSON.stringify(t.value)}${tail}`;
  }
  throw new Error(`a ${t.termType}`);
};
const line = (q: Quad) => `${term(q.subject)} ${term(q.predicate)} ${term(q.object)}`;

/** `people.ttl` as it reads back: bob's age and `active` in their canonical forms. */
const canonical = async (): Promise<string[]> =>
  new Parser()
    .parse(await readFile(join(DATA, 'people.ttl'), 'utf8'))
    .map(line)
    .map((l) => l.replace(`"031"^^<${XSD}integer>`, `"31"^^<${XSD}integer>`).replace(`"0"^^<${XSD}boolean>`, `"false"^^<${XSD}boolean>`))
    .sort();

const all = async (r: Rdf, pattern?: TriplePattern): Promise<string[]> => {
  const out: string[] = [];
  for await (const q of r.triples(pattern)) out.push(line(q));
  return out.sort();
};

describe.each([
  ['shacl', 'people.shacl.ttl'],
  ['shexj', 'people.shex.json'],
  ['shexc', 'people.shex'],
] as const)('attach over the graph written with %s', async (syntax, source) => {
  const e = await engine();
  const g = await attach(e, `rdf_${syntax}`, under(join(RDF, syntax)), { entry });
  const r = g.rdf!;

  it('reads back every triple it was written from, in canonical form', async () => {
    const expected = await canonical();
    const got = await all(r);
    expect(got).toEqual(expected);
    expect(new Set(got).size).toBe(got.length);
  });

  it('keeps the shape document and the mapping exactly as they were saved', async () => {
    expect(r.shapes).toEqual({ syntax, text: await readFile(join(DATA, source), 'utf8') });
    expect(r.mapping).toBe(await readFile(join(RDF, syntax, 'mapping.ttl'), 'utf8'));
  });

  it('reads a triple pattern from only the maps that can make it', async () => {
    const { namedNode, literal } = DataFactory;
    const ex = (p: string) => namedNode(`https://example.org/${p}`);
    const every = await all(r);
    const patterns: [TriplePattern, (l: string) => boolean][] = [
      [{ subject: ex('alice') }, (l) => l.startsWith('<https://example.org/alice> ')],
      [{ predicate: namedNode('http://xmlns.com/foaf/0.1/name') }, (l) => l.includes(' <http://xmlns.com/foaf/0.1/name> ')],
      [{ predicate: ex('knows'), object: ex('alice') }, (l) => l.endsWith(' <https://example.org/knows> <https://example.org/alice>')],
      [
        { predicate: namedNode('http://www.w3.org/1999/02/22-rdf-syntax-ns#type'), object: ex('Organization') },
        (l) => l.endsWith(' <https://example.org/Organization>'),
      ],
      [{ object: literal('31', namedNode(`${XSD}integer`)) }, (l) => l.endsWith(` "31"^^<${XSD}integer>`)],
      [{ object: literal('Alice') }, (l) => l.endsWith(' "Alice"')],
      [{ predicate: ex('nothing') }, () => false],
    ];
    for (const [pattern, keep] of patterns) {
      const expected = every.filter(keep);
      expect(await all(r, pattern)).toEqual(expected);
    }
    expect(every.filter((l) => l.endsWith(' "Alice"'))).toHaveLength(1);
    // A predicate only one edge type has reads only that edge type's relation.
    const { sql } = r.match({ predicate: ex('knows') });
    const body = sql.slice(sql.indexOf(') SELECT ') + 2);
    expect(body).toContain('"Person_knows_Person"');
    expect(body).not.toContain('"Organization"');
  });

  it('says what the mapping says of every type, property and edge type', () => {
    expect(r.vertexTypes.map((v) => [v.type, v.iri, v.shape])).toEqual([
      ['Organization', 'https://example.org/Organization', 'https://example.org/shapes/Organization'],
      ['Person', 'https://example.org/Person', 'https://example.org/shapes/Person'],
    ]);
    const person = r.vertexTypes.find((v) => v.type === 'Person')!;
    const p = (name: string) => {
      const { iri, termType, datatype } = person.properties.find((x) => x.name === name)!;
      return { iri, termType, datatype };
    };
    expect(p('subject')).toEqual({ iri: undefined, termType: 'IRI', datatype: undefined });
    expect(p('name')).toEqual({ iri: 'http://xmlns.com/foaf/0.1/name', termType: 'Literal', datatype: `${XSD}string` });
    expect(p('name_2')).toEqual({ iri: 'https://schema.org/name', termType: 'Literal', datatype: `${XSD}string` });
    expect(p('age')).toEqual({ iri: 'https://example.org/age', termType: 'Literal', datatype: `${XSD}integer` });
    expect(p('homepage')).toEqual({ iri: 'https://example.org/homepage', termType: 'IRI', datatype: undefined });
    // A set and a single tagged string each have their own triples map, matched by its name.
    expect(p('nick')).toEqual({
      iri: 'https://example.org/nick', termType: 'Literal', datatype: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString',
    });
    expect(p('tag')).toEqual({ iri: 'https://example.org/tag', termType: 'Literal', datatype: `${XSD}string` });
    expect(p('motto').datatype).toBe('http://www.w3.org/1999/02/22-rdf-syntax-ns#langString');
    expect(r.edgeTypes.map((t) => [t.name, t.iri])).toEqual([
      ['Person_knows_Person', 'https://example.org/knows'],
      ['Person_worksFor_Organization', 'https://example.org/worksFor'],
    ]);
  });

  it('labels what the shapes label, and nothing else', () => {
    const person = r.vertexTypes.find((v) => v.type === 'Person')!;
    const organization = r.vertexTypes.find((v) => v.type === 'Organization')!;
    const knows = r.edgeTypes.find((t) => t.edgeType === 'knows')!;
    const label = (name: string) => {
      const { label, description } = person.properties.find((x) => x.name === name)!;
      return { label, description };
    };
    if (syntax === 'shexc') {
      // No ShExC parser here: a ShExC document gives no labels.
      expect([person.label, label('name').label, knows.label]).toEqual([undefined, undefined, undefined]);
      return;
    }
    // The untagged one, else `en`.
    expect([person.label, person.description]).toEqual(['Person', 'Someone the graph knows of.']);
    expect(label('name')).toEqual({ label: 'name', description: 'What the person is called.' });
    expect(label('nick')).toEqual({ label: 'nickname', description: undefined });
    expect(label('age')).toEqual({ label: undefined, description: undefined });
    expect(knows.label).toBe('knows');
    expect([organization.label, organization.description]).toEqual([undefined, undefined]);
  });

  it('is still the Graph attach answers for any graph', async () => {
    expect(await g.vertex('Person', 0)).toMatchObject({ subject: 'https://example.org/alice', name: 'Alice' });
    expect(g.vertexTypes.map((v) => v.count)).toEqual([1, 4]);
  });
});

describe('attach and the mapping', async () => {
  const e = await engine();

  it('reads no RDF where the graph document names no mapping', async () => {
    await using sample = await attach(e, 'sample', under(SAMPLE), { entry: 'sample.graph.yml' });
    expect(sample.rdf).toBeUndefined();
  });

  it('refuses a mapping the graph document names and that does not read, and gives the catalog back', async () => {
    const dir = await copy(join(RDF, 'shacl'), 'mapping.ttl', null);
    const err = await attach(e, 'none', under(dir), { entry }).catch((x: unknown) => x);
    expect(err).toBeInstanceOf(GraphArError);
    expect((err as GraphArError).code).toBe('unreadable');
    expect(await e.rows(`SELECT * FROM duckdb_databases() WHERE database_name = 'none'`)).toEqual([]);
  });

  it.each([
    ['a graph map', (t: string) => t.replace('rml:subjectMap [ rml:reference "subject" ; rml:termType rml:IRI ;', 'rml:subjectMap [ rml:reference "subject" ; rml:termType rml:IRI ; rml:graphMap [ rml:constant <urn:g> ] ;'), 'unsupported'],
    ['a template', (t: string) => t.replace('rml:reference "founded"', 'rml:template "{founded}"'), 'unsupported'],
    ['a triple outside the maps', (t: string) => `${t}\n<urn:a> <urn:b> <urn:c> .\n`, 'unsupported'],
    ['a map of no type', (t: string) => t.replace('<#Organization/name>', '<#Nothing/name>'), 'invalid'],
    ['Turtle that is not', (t: string) => `${t} <`, 'unparseable'],
  ])('a mapping with %s', async (_, edit, code) => {
    const dir = await copy(join(RDF, 'shacl'), 'mapping.ttl', edit);
    const err = await attach(e, 'refused', under(dir), { entry }).catch((x: unknown) => x);
    expect(err).toBeInstanceOf(GraphArError);
    expect((err as GraphArError).code).toBe(code);
    // The catalog is given back.
    expect(await e.rows(`SELECT * FROM duckdb_databases() WHERE database_name = 'refused'`)).toEqual([]);
  });
});
