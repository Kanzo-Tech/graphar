/**
 * `conformance/sample`, the graph `graphar-parquet` writes and GraphAr's C++ library reads in CI,
 * attached to a native DuckDB. Its values are `crates/graphar-parquet/tests/sample/mod.rs`'s.
 */

import { cp, mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve as resolvePath } from 'node:path';

import { afterAll, describe, expect, it } from 'vitest';

import { attachGraph, GraphArError, type Resolve } from '../src/index.js';
import { engine } from './engine.js';

const SAMPLE = resolvePath(import.meta.dirname, '../../../conformance/sample');
const USER_DEFINED = resolvePath(import.meta.dirname, '../../../conformance/user_defined');
const under = (dir: string): Resolve => (paths) => paths.map((p) => `${dir}/${p}`);
const entry = 'sample.graph.yml';

const temporary: string[] = [];
afterAll(() => Promise.all(temporary.map((d) => rm(d, { recursive: true, force: true }))));

/** A copy of the sample, with `edit` applied to one of its documents. */
async function copy(file: string, edit: (text: string) => string, from = SAMPLE): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), 'graphar-duckdb-'));
  temporary.push(dir);
  await cp(from, dir, { recursive: true });
  await writeFile(join(dir, file), edit(await readFile(join(dir, file), 'utf8')));
  return dir;
}

const num = (rows: Record<string, unknown>[]) =>
  rows.map((r) => Object.fromEntries(Object.entries(r).map(([k, v]) => [k, typeof v === 'bigint' ? Number(v) : v])));

describe('attachGraph over the sample', async () => {
  const e = await engine();
  const g = await attachGraph(e, 'g', under(SAMPLE), { entry });

  it('parses the information files', () => {
    expect(g.graph.name).toBe('sample');
    expect(g.graph.prefix).toBeUndefined();
    expect(g.graph.vertices.map((v) => v.type)).toEqual(['person', 'software', 'nickname']);
    expect(g.graph.edges[0]!.adj_lists.map((a) => a.type)).toEqual(['ordered_by_source', 'unordered_by_dest']);
  });

  it('views a vertex type with its groups joined on the internal id', async () => {
    const rows = num(await e.rows(`SELECT * FROM "g"."person" ORDER BY _graphArVertexIndex`));
    expect(rows.map((r) => r['_graphArVertexIndex'])).toEqual([0, 1, 2, 3, 4, 5, 6]);
    expect(rows.map((r) => r['name'])).toEqual(['ana', 'bo', 'cy', 'di', 'ed', 'flo', 'gus']);
    expect(rows.map((r) => r['id'])).toEqual([100, 101, 102, 103, 104, 105, 106]);
    expect(Object.keys(rows[0]!)).toEqual(['_graphArVertexIndex', 'id', 'name', 'age', 'born', 'active']);
    expect(rows[2]!['age']).toBeNull();
    expect(rows[4]!['active']).toBeNull();
  });

  it('views a vertex type with no primary key like any other', async () => {
    const rows = num(await e.rows(`SELECT src, value FROM "g"."nickname" ORDER BY _graphArVertexIndex`));
    expect(rows).toEqual([{ src: 0, value: 'annie' }, { src: 0, value: 'an' }, { src: 3, value: 'dee' }]);
  });

  it('views an edge type with each property row for row beside its edge', async () => {
    const rows = num(await e.rows(`SELECT * FROM "g"."person_knows_person" ORDER BY _graphArSrcIndex, _graphArDstIndex`));
    expect(rows).toEqual(
      [
        [0, 1, 0.5], [0, 2, 0.25], [0, 6, 1.5], [1, 0, 0.125], [1, 2, 0.75], [2, 6, 1.0], [6, 0, 3.0], [6, 5, 2.0],
      ].map(([s, d, w]) => ({ _graphArSrcIndex: s, _graphArDstIndex: d, weight: w })),
    );
    const created = num(await e.rows(`SELECT * FROM "g"."person_created_software" ORDER BY ALL`));
    expect(created.map((r) => [r['_graphArSrcIndex'], r['_graphArDstIndex']])).toEqual([[0, 0], [2, 1], [6, 0], [6, 2]]);
  });

  it('holds the information files as four relations', async () => {
    expect(num(await e.rows(`SELECT * FROM "g".graphar_vertex_types ORDER BY ordinal`))).toEqual([
      { type: 'person', count: 7, ordinal: 1 },
      { type: 'software', count: 3, ordinal: 2 },
      { type: 'nickname', count: 3, ordinal: 3 },
    ]);
    expect(num(await e.rows(`SELECT * FROM "g".graphar_edge_types ORDER BY ordinal`))).toEqual([
      { name: 'person_knows_person', src_type: 'person', edge_type: 'knows', dst_type: 'person', count: 8, directed: true, ordinal: 1 },
      { name: 'person_created_software', src_type: 'person', edge_type: 'created', dst_type: 'software', count: 4, directed: true, ordinal: 2 },
    ]);
    const props = num(await e.rows(`SELECT * FROM "g".graphar_properties WHERE type = 'person' ORDER BY ordinal`));
    expect(props.map((p) => [p['property'], p['data_type'], p['is_primary'], p['is_nullable'], p['ordinal']])).toEqual([
      ['id', 'int64', true, false, 1],
      ['name', 'string', false, false, 2],
      ['age', 'int32', false, true, 3],
      ['born', 'date', false, true, 4],
      ['active', 'bool', false, true, 5],
    ]);
    expect(await e.rows(`SELECT * FROM "g".graphar_extra_info`)).toEqual([{ key: 'written_by', value: 'graphar-parquet' }]);
  });

  it('holds the chunk files by the names resolve gave, one row each', async () => {
    const rows = await e.rows(`SELECT type, kind, path FROM "g".graphar_chunks`);
    const count = (type: string, kind: string) => rows.filter((r) => r['type'] === type && r['kind'] === kind).length;
    expect(rows.every((r) => String(r['path']).startsWith(`${SAMPLE}/`))).toBe(true);
    expect(count('person', 'vertex')).toBe(6); // 3 chunks of 3, two groups
    expect(count('software', 'vertex')).toBe(2);
    expect(count('nickname', 'vertex')).toBe(1);
    expect(count('person_knows_person', 'offset')).toBe(3); // a chunk per vertex chunk
    expect(count('person_knows_person', 'adj_list')).toBe(3); // the empty part has none
    expect(count('person_knows_person', 'edge_property')).toBe(3);
    expect(count('person_created_software', 'offset')).toBe(3);
    expect(rows.length).toBe(6 + 2 + 1 + 3 + 3 + 3 + 3 + 2);
    expect(new Set(rows.map((r) => r['kind']))).toEqual(new Set(['vertex', 'offset', 'adj_list', 'edge_property']));
    // A consumer reads a chunk's footer by that name.
    const [first] = rows.filter((r) => r['kind'] === 'adj_list');
    const meta = num(await e.rows(`SELECT sum(num_rows)::INTEGER AS n FROM parquet_file_metadata('${first!['path']}')`));
    expect(meta[0]!['n']).toBeGreaterThan(0);
    // Every listed path is a Parquet file.
    for (const r of rows) {
      expect(num(await e.rows(`SELECT count(*)::INTEGER AS n FROM parquet_file_metadata('${r['path']}')`))[0]!['n']).toBe(1);
    }
  });

  it('declares the type of every column of the five relations', async () => {
    const columns = async (relation: string) =>
      (await e.rows(`SELECT column_name, column_type FROM (DESCRIBE "g".${relation})`)).map((r) => [r['column_name'], r['column_type']]);
    expect(await columns('graphar_vertex_types')).toEqual([['type', 'VARCHAR'], ['count', 'UBIGINT'], ['ordinal', 'INTEGER']]);
    expect(await columns('graphar_edge_types')).toEqual([
      ['name', 'VARCHAR'], ['src_type', 'VARCHAR'], ['edge_type', 'VARCHAR'], ['dst_type', 'VARCHAR'], ['count', 'UBIGINT'],
      ['directed', 'BOOLEAN'], ['ordinal', 'INTEGER'],
    ]);
    expect(await columns('graphar_properties')).toEqual([
      ['type', 'VARCHAR'], ['property', 'VARCHAR'], ['data_type', 'VARCHAR'], ['is_primary', 'BOOLEAN'], ['is_nullable', 'BOOLEAN'],
      ['ordinal', 'INTEGER'], ['cardinality', 'VARCHAR'],
    ]);
    expect(await columns('graphar_extra_info')).toEqual([['key', 'VARCHAR'], ['value', 'VARCHAR']]);
    expect(await columns('graphar_chunks')).toEqual([['type', 'VARCHAR'], ['kind', 'VARCHAR'], ['path', 'VARCHAR']]);
  });

  it('gives a reader one id space by arithmetic over the relations', async () => {
    // What `@kanzo-tech/graph` computes: first(t) is the sum of the counts of the types before it.
    const rows = num(await e.rows(`
      WITH first AS (SELECT type, sum(count) OVER (ORDER BY ordinal) - count AS first FROM "g".graphar_vertex_types)
      SELECT (f.first + s._graphArVertexIndex)::UINTEGER AS dense_id, s.name
      FROM "g"."software" s, first f WHERE f.type = 'software' ORDER BY dense_id`));
    expect(rows).toEqual([{ dense_id: 7, name: 'lop' }, { dense_id: 8, name: 'ripple' }, { dense_id: 9, name: 'nebula' }]);
  });

  it('detaches, and a second detach does nothing', async () => {
    await g.detach();
    await g.detach();
    await expect(e.rows(`SELECT * FROM "g"."person"`)).rejects.toThrow();
  });
});

describe('attachGraph', () => {
  it('reads every file through the engine, a few statements and resolutions per attach', async () => {
    const e = await engine();
    const asked: (readonly string[])[] = [];
    await using _g = await attachGraph(e, 'g', (paths) => (asked.push(paths), under(SAMPLE)(paths)), { entry });
    // The graph document; the documents it names; the type counts; the part counts; the chunks.
    expect(asked.length).toBe(5);
    expect(asked[0]).toEqual([entry]);
    expect(asked[1]).toContain('person_knows_person.edge.yml');
    expect(asked.flat().every((p) => !p.startsWith('/') && !p.startsWith('.'))).toBe(true);
    expect(e.statements.filter((s) => s.startsWith('SELECT')).length).toBe(5);
  });

  it('keeps a catalog two attachments share until the last detaches', async () => {
    const e = await engine();
    const one = await attachGraph(e, 'shared', under(SAMPLE), { entry });
    const two = await attachGraph(e, 'shared', under(SAMPLE), { entry });
    await one.detach();
    expect(await e.rows(`SELECT count(*)::INTEGER AS n FROM "shared"."person"`)).toEqual([{ n: 7 }]);
    await two.detach();
    await expect(e.rows(`SELECT * FROM "shared"."person"`)).rejects.toThrow();
  });

  it('reads a graph under a directory of the entry', async () => {
    const e = await engine();
    const parent = resolvePath(SAMPLE, '..');
    await using g = await attachGraph(e, 'sub', under(parent), { entry: 'sample/sample.graph.yml' });
    expect(await e.rows(`SELECT count(*)::INTEGER AS n FROM "sub"."person_knows_person"`)).toEqual([{ n: 8 }]);
    expect(g.graph.vertices).toHaveLength(3);
  });

  it('views an empty type with its columns typed', async () => {
    const dir = await copy(entry, (t) => t);
    const zero = Buffer.alloc(8);
    await writeFile(join(dir, 'vertex/nickname/vertex_count'), zero);
    for (const p of [0, 1, 2]) await writeFile(join(dir, `edge/person_created_software/ordered_by_source/edge_count${p}`), zero);
    const e = await engine();
    await using _g = await attachGraph(e, 'empty', under(dir), { entry });
    expect(await e.rows(`SELECT count(*)::INTEGER AS n FROM "empty"."nickname"`)).toEqual([{ n: 0 }]);
    expect(await e.rows(`SELECT column_name, column_type AS data_type FROM (DESCRIBE "empty"."nickname")`)).toEqual([
      { column_name: '_graphArVertexIndex', data_type: 'BIGINT' },
      { column_name: 'src', data_type: 'BIGINT' },
      { column_name: 'value', data_type: 'VARCHAR' },
    ]);
    expect(await e.rows(`SELECT count(*)::INTEGER AS n FROM "empty"."person_created_software"`)).toEqual([{ n: 0 }]);
    expect(await e.rows(`SELECT column_type FROM (DESCRIBE "empty".graphar_chunks)`)).toHaveLength(3);
    expect(num(await e.rows(`SELECT count FROM "empty".graphar_edge_types WHERE edge_type = 'created'`))).toEqual([{ count: 0 }]);
  });

  it('refuses a count file that is not one int64', async () => {
    const dir = await copy(entry, (t) => t);
    await writeFile(join(dir, 'vertex/person/vertex_count'), Buffer.alloc(4));
    await refusal(dir, 'invalid', /vertex\/person\/vertex_count: not a count/);
  });

  const refusal = async (dir: string, code: GraphArError['code'], message: RegExp) => {
    const e = await engine();
    const attaching = attachGraph(e, 'bad', under(dir), { entry });
    await expect(attaching).rejects.toBeInstanceOf(GraphArError);
    await expect(attaching).rejects.toMatchObject({ code, message: expect.stringMatching(message) });
    // Nothing is left behind.
    expect(await e.rows(`SELECT count(*)::INTEGER AS n FROM duckdb_databases() WHERE database_name = 'bad'`)).toEqual([{ n: 0 }]);
  };

  it('refuses a version GraphAr does not define', async () => {
    await refusal(await copy(entry, (t) => t.replace('gar/v1', 'gar/v2')), 'invalid', /version `gar\/v2`/);
  });

  it('refuses a key GraphAr does not define', async () => {
    await refusal(await copy('person.vertex.yml', (t) => `${t}stats: true\n`), 'invalid', /`stats` is not a key/);
  });

  it('refuses a document that is not YAML', async () => {
    await refusal(await copy('software.vertex.yml', (t) => `${t}\n  - : [\n`), 'unparseable', /software\.vertex\.yml/);
  });

  it('refuses a file that does not read', async () => {
    await refusal(await copy(entry, (t) => t.replace('nickname.vertex.yml', 'missing.vertex.yml')), 'unreadable', /missing\.vertex\.yml/);
  });

  it('refuses a type that would hide a relation', async () => {
    const dir = await copy('nickname.vertex.yml', (t) => t.replace('type: nickname', 'type: graphar_properties'));
    await refusal(dir, 'duplicate', /`graphar_properties` names two relations/);
  });

  it('refuses a file type other than Parquet', async () => {
    await refusal(await copy('software.vertex.yml', (t) => t.replace('parquet', 'csv')), 'unsupported', /`csv` is not Parquet/);
  });

  it('stops on an abort, with the reason', async () => {
    const e = await engine();
    const stop = new AbortController();
    const reason = new Error('stopped');
    const attaching = attachGraph(e, 'stopped', (paths) => (stop.abort(reason), under(SAMPLE)(paths)), { entry, signal: stop.signal });
    await expect(attaching).rejects.toBe(reason);
  });
});

describe('attachGraph over user_defined: a list of a user-defined type', () => {
  // `gar/v1 (langString)` declares the type; GraphAr gives it no storage. graphar-parquet wrote it
  // as LIST<STRUCT<value: STRING, lang: STRING>>, and the view reads that from the file.
  const user = 'user_defined.graph.yml';

  it('reads the column as a DuckDB LIST of STRUCT, and says its type in GraphAr\'s words', async () => {
    const e = await engine();
    await using g = await attachGraph(e, 'u', under(USER_DEFINED), { entry: user });
    expect(g.graph.version).toBe('gar/v1 (langString)');
    const types = await e.rows(`SELECT column_name, column_type FROM (DESCRIBE "u"."concept")`);
    expect(types).toEqual([
      { column_name: '_graphArVertexIndex', column_type: 'BIGINT' },
      { column_name: 'id', column_type: 'BIGINT' },
      { column_name: 'labels', column_type: 'STRUCT("value" VARCHAR, lang VARCHAR)[]' },
    ]);
    const rows = await e.rows(`SELECT id::INTEGER AS id, labels FROM "u"."concept" ORDER BY id`);
    expect(rows).toEqual([
      { id: 1, labels: [{ value: 'cat', lang: 'en' }, { value: 'gato', lang: 'es' }] },
      { id: 2, labels: [] },
      { id: 3, labels: null },
    ]);
    expect(
      await e.rows(`SELECT unnest(labels).lang AS lang FROM "u"."concept" WHERE id = 1 ORDER BY ALL`),
    ).toEqual([{ lang: 'en' }, { lang: 'es' }]);
    expect(await e.rows(`SELECT property, data_type, is_nullable, cardinality FROM "u".graphar_properties ORDER BY ordinal`)).toEqual([
      { property: 'id', data_type: 'int64', is_nullable: false, cardinality: 'single' },
      { property: 'labels', data_type: 'langString', is_nullable: true, cardinality: 'set' },
    ]);
  });

  it('types an empty view only if the host says what the user-defined type is', async () => {
    const dir = await copy('concept.vertex.yml', (t) => t, USER_DEFINED);
    await writeFile(join(dir, 'vertex/concept/vertex_count'), Buffer.alloc(8));
    const e = await engine();
    const without = attachGraph(e, 'u', under(dir), { entry: user });
    await expect(without).rejects.toMatchObject({ code: 'unsupported', message: expect.stringMatching(/`langString` has no DuckDB type/) });

    const userTypes = { langString: 'STRUCT(value VARCHAR, lang VARCHAR)' };
    await using _g = await attachGraph(e, 'u', under(dir), { entry: user, userTypes });
    expect(await e.rows(`SELECT column_type FROM (DESCRIBE "u"."concept") WHERE column_name = 'labels'`)).toEqual([
      { column_type: 'STRUCT("value" VARCHAR, lang VARCHAR)[]' },
    ]);
  });
});

describe('attachGraph and the internal vertex id', () => {
  // "starting from 0, corresponding to the row number of the vertex in the logical vertex table"
  // (format.md): a gap would make a prefix-sum global id collide with the next type's first.
  it('refuses a type whose ids have a gap, naming it', async () => {
    const dir = await copy(entry, (t) => t);
    const e = await engine();
    // `person`'s last chunk holds id 6; make it 7, so 7 rows run 0..7.
    const chunk = join(dir, 'vertex/person/id/chunk2');
    await e.rows(`COPY (SELECT _graphArVertexIndex + 1 AS _graphArVertexIndex, id FROM read_parquet('${chunk}')) TO '${chunk}.new' (FORMAT parquet)`);
    await rename(`${chunk}.new`, chunk);
    const attaching = attachGraph(e, 'gap', under(dir), { entry });
    await expect(attaching).rejects.toMatchObject({ code: 'invalid', document: 'person' });
    await expect(attaching).rejects.toThrow(/vertex `person`.*not continuous.*ids from 0 to 7/);
    expect(await e.rows(`SELECT count(*)::INTEGER AS n FROM duckdb_databases() WHERE database_name = 'gap'`)).toEqual([{ n: 0 }]);
  });

  it('refuses ids that start above 0, and rows that are not the count', async () => {
    const e = await engine();
    const shifted = await copy(entry, (t) => t);
    for (const k of [0, 1, 2]) {
      const chunk = join(shifted, `vertex/person/id/chunk${k}`);
      await e.rows(`COPY (SELECT _graphArVertexIndex + 1 AS _graphArVertexIndex, id FROM read_parquet('${chunk}')) TO '${chunk}.new' (FORMAT parquet)`);
      await rename(`${chunk}.new`, chunk);
    }
    await expect(attachGraph(e, 'a', under(shifted), { entry })).rejects.toMatchObject({ code: 'invalid', document: 'person' });
    const short = await copy(entry, (t) => t);
    await writeFile(join(short, 'vertex/software/vertex_count'), Buffer.from([4, 0, 0, 0, 0, 0, 0, 0]));
    await expect(attachGraph(e, 'b', under(short), { entry })).rejects.toMatchObject({ code: 'invalid', document: 'software' });
  });
});
