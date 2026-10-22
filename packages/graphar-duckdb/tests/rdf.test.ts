/**
 * `conformance/rdf`, the graphs `graphar-rdf`'s example writes from
 * `crates/graphar-rdf/tests/data/people.ttl` and each of its three shape documents, attached to a
 * native DuckDB and read back as RDF.
 */

import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve as resolvePath } from 'node:path';

import type { Quad, Term } from '@rdfjs/types';
import { Parser } from 'n3';
import { afterAll, describe, expect, it } from 'vitest';

import { attachRdfGraph, GraphArError, type RdfGraph, type Resolve } from '../src/index.js';
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

const all = async (g: RdfGraph): Promise<string[]> => {
  const out: string[] = [];
  for await (const q of g.triples()) out.push(line(q));
  return out.sort();
};

describe.each([
  ['shacl', 'people.shacl.ttl'],
  ['shexj', 'people.shex.json'],
  ['shexc', 'people.shex'],
] as const)('attachRdfGraph over the graph written with %s', async (syntax, source) => {
  const e = await engine();
  const g = await attachRdfGraph(e, `rdf_${syntax}`, under(join(RDF, syntax)), { entry });

  it('reads back every triple it was written from, in canonical form', async () => {
    const expected = await canonical();
    const got = await all(g);
    expect(got).toEqual(expected);
    expect(new Set(got).size).toBe(got.length);
  });

  it('keeps the shape document exactly as it was given', async () => {
    expect(g.shapes).toEqual({ syntax, text: await readFile(join(DATA, source), 'utf8') });
  });

  it('says what the mapping says of every type, property and edge type', () => {
    expect(g.vertexTypes.map((v) => [v.type, v.iri, v.shape])).toEqual([
      ['Organization', 'https://example.org/Organization', 'https://example.org/shapes/Organization'],
      ['Person', 'https://example.org/Person', 'https://example.org/shapes/Person'],
    ]);
    const person = g.vertexTypes.find((v) => v.type === 'Person')!;
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
    expect(g.edgeTypes.map((t) => [t.name, t.iri])).toEqual([
      ['Person_knows_Person', 'https://example.org/knows'],
      ['Person_worksFor_Organization', 'https://example.org/worksFor'],
    ]);
  });

  it('labels what the shapes label, and nothing else', () => {
    const person = g.vertexTypes.find((v) => v.type === 'Person')!;
    const organization = g.vertexTypes.find((v) => v.type === 'Organization')!;
    const knows = g.edgeTypes.find((t) => t.edgeType === 'knows')!;
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

  it('is still the Graph attachGraph answers', async () => {
    expect(await g.vertex('Person', 0)).toMatchObject({ subject: 'https://example.org/alice', name: 'Alice' });
    expect(g.vertexTypes.map((v) => v.count)).toEqual([1, 4]);
  });
});

describe('attachRdfGraph refuses', async () => {
  const e = await engine();

  it('a graph with no mapping, before it attaches anything', async () => {
    const dir = await copy(join(RDF, 'shacl'), 'mapping.ttl', null);
    const before = e.statements.length;
    const err = await attachRdfGraph(e, 'none', under(dir), { entry }).catch((x: unknown) => x);
    expect(err).toBeInstanceOf(GraphArError);
    expect((err as GraphArError).code).toBe('unreadable');
    expect((err as GraphArError).document).toBe('mapping.ttl');
    expect(e.statements.slice(before).some((s) => s.startsWith('ATTACH'))).toBe(false);
    const sample = await attachRdfGraph(e, 'sample', under(SAMPLE), { entry: 'sample.graph.yml' }).catch((x: unknown) => x);
    expect((sample as GraphArError).code).toBe('unreadable');
  });

  it.each([
    ['a graph map', (t: string) => t.replace('rml:subjectMap [ rml:reference "subject" ; rml:termType rml:IRI ;', 'rml:subjectMap [ rml:reference "subject" ; rml:termType rml:IRI ; rml:graphMap [ rml:constant <urn:g> ] ;'), 'unsupported'],
    ['a template', (t: string) => t.replace('rml:reference "founded"', 'rml:template "{founded}"'), 'unsupported'],
    ['a triple outside the maps', (t: string) => `${t}\n<urn:a> <urn:b> <urn:c> .\n`, 'unsupported'],
    ['a map of no type', (t: string) => t.replace('<#Organization/name>', '<#Nothing/name>'), 'invalid'],
    ['Turtle that is not', (t: string) => `${t} <`, 'unparseable'],
  ])('a mapping with %s', async (_, edit, code) => {
    const dir = await copy(join(RDF, 'shacl'), 'mapping.ttl', edit);
    const err = await attachRdfGraph(e, 'refused', under(dir), { entry }).catch((x: unknown) => x);
    expect(err).toBeInstanceOf(GraphArError);
    expect((err as GraphArError).code).toBe(code);
    // The catalog is given back.
    expect(await e.rows(`SELECT * FROM duckdb_databases() WHERE database_name = 'refused'`)).toEqual([]);
  });
});
