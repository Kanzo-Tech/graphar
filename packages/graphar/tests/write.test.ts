/**
 * `@kanzo-tech/graphar/write`: `people.ttl` and each of its shape documents written through the
 * wasm32 writer into a directory, attached back on a native DuckDB, and read as RDF.
 */

import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve as resolvePath } from 'node:path';

import type { Quad, Term } from '@rdfjs/types';
import { Parser } from 'n3';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { attach, type Shapes } from '../src/index.js';
import { initWriter, type Put, write } from '../src/write.js';
import { engine } from './engine.js';

const DATA = resolvePath(import.meta.dirname, '../../../crates/graphar-rdf/tests/data');
const XSD = 'http://www.w3.org/2001/XMLSchema#';

const temporary: string[] = [];
afterAll(() => Promise.all(temporary.map((d) => rm(d, { recursive: true, force: true }))));
beforeAll(async () => {
  await initWriter(await readFile(resolvePath(import.meta.dirname, '../pkg/graphar_rdf_wasm_bg.wasm')));
});

const term = (t: Term): string => {
  if (t.termType === 'NamedNode') return `<${t.value}>`;
  if (t.termType === 'Literal') {
    const tail = t.language !== '' ? `@${t.language}` : t.datatype.value === `${XSD}string` ? '' : `^^<${t.datatype.value}>`;
    return `${JSON.stringify(t.value)}${tail}`;
  }
  throw new Error(`a ${t.termType}`);
};
const line = (q: Quad) => `${term(q.subject)} ${term(q.predicate)} ${term(q.object)}`;

const people = async (): Promise<Quad[]> => new Parser().parse(await readFile(join(DATA, 'people.ttl'), 'utf8'));

/** A directory, and the `put` that writes into it. */
async function directory(): Promise<{ dir: string; put: Put; paths: string[] }> {
  const dir = await mkdtemp(join(tmpdir(), 'graphar-write-'));
  temporary.push(dir);
  const paths: string[] = [];
  const put: Put = async (path, bytes) => {
    paths.push(path);
    await mkdir(dirname(join(dir, path)), { recursive: true });
    await writeFile(join(dir, path), bytes);
  };
  return { dir, put, paths };
}

describe.each([
  ['shacl', 'people.shacl.ttl'],
  ['shexc', 'people.shex'],
  ['shexj', 'people.shex.json'],
] as const)('write with %s, read back with attach', (syntax, file) => {
  it('gives back every triple, in canonical form, the graph document put last', async () => {
    const shapes: Shapes = { syntax, text: await readFile(join(DATA, file), 'utf8') };
    const { dir, put, paths } = await directory();
    const written = await write(put, { rdf: { shapes, triples: await people() } }, { chunkSize: 2 });
    expect(written).toEqual({
      vertices: [['Organization', 1], ['Person', 4]],
      edges: [['Person_knows_Person', 4], ['Person_worksFor_Organization', 2]],
      triples: 31,
    });
    expect(paths.at(-1)).toBe('graph.graph.yml');
    expect(paths).toContain('vertex/Person/properties/chunk1');

    const e = await engine();
    await using g = await attach(e, `written_${syntax}`, (ps) => ps.map((p) => `${dir}/${p}`), { entry: 'graph.graph.yml' });
    const back: string[] = [];
    for await (const q of g.rdf!.triples()) back.push(line(q));
    const given = (await people())
      .map(line)
      .map((l) => l.replace(`"031"^^<${XSD}integer>`, `"31"^^<${XSD}integer>`).replace(`"0"^^<${XSD}boolean>`, `"false"^^<${XSD}boolean>`));
    expect(back.sort()).toEqual(given.sort());
    expect(g.rdf!.shapes).toEqual(shapes);
  });
});

describe('write refuses', () => {
  const shacl = async (): Promise<Shapes> => ({ syntax: 'shacl', text: await readFile(join(DATA, 'people.shacl.ttl'), 'utf8') });

  it('a graph that does not conform, and puts no graph document', async () => {
    const { put, paths } = await directory();
    // A second name for carol, among carol's triples.
    const [extra] = new Parser().parse('<https://example.org/carol> <http://xmlns.com/foaf/0.1/name> "Caroline" .');
    const all = await people();
    const at = all.findIndex((q) => q.subject.value === 'https://example.org/carol');
    const triples = [...all.slice(0, at), extra!, ...all.slice(at)];
    const err = await write(put, { rdf: { shapes: await shacl(), triples } }).catch((x: unknown) => x);
    expect(String(err)).toContain('Person.name has at most one value');
    expect(paths).not.toContain('graph.graph.yml');
  });

  it('a subject whose triples are not together', async () => {
    const { put } = await directory();
    const all = await people();
    const alice = all.filter((q) => q.subject.value === 'https://example.org/alice');
    const rest = all.filter((q) => q.subject.value !== 'https://example.org/alice');
    // alice's class first, and again with the rest of her triples after everyone else's.
    const type = alice.filter((q) => q.predicate.value.endsWith('#type'));
    const triples = [...alice, ...rest, ...type];
    const err = await write(put, { rdf: { shapes: await shacl(), triples } }).catch((x: unknown) => x);
    expect(String(err)).toContain('<https://example.org/alice> reappears');
  });

  it('a put that rejects, with its reason', async () => {
    const put: Put = async () => {
      throw new Error('the store is full');
    };
    const err = await write(put, { rdf: { shapes: await shacl(), triples: await people() } }, { chunkSize: 1 }).catch((x: unknown) => x);
    expect(String(err)).toContain('the store is full');
  });
});
