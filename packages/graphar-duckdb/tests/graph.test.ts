/**
 * The `Graph` `attachGraph` answers, over `conformance/sample`: its values are
 * `crates/graphar-parquet/tests/sample/mod.rs`'s.
 */

import { resolve as resolvePath } from 'node:path';

import { describe, expect, it } from 'vitest';

import { attachGraph, GraphArError, type EdgeType, type VertexType } from '../src/index.js';
import { graphOf } from '../src/graph.js';
import { engine } from './engine.js';

const SAMPLE = resolvePath(import.meta.dirname, '../../../conformance/sample');

describe('Graph over the sample', async () => {
  const e = await engine();
  const g = await attachGraph(e, 'g', (paths) => paths.map((p) => `${SAMPLE}/${p}`), { entry: 'sample.graph.yml' });
  const rows = async (sql: string) => (await e.rows(sql)).map((r) => Object.values(r));

  it('describes every property with its DuckDB type and its Parquet statistics', () => {
    const [person, software, nickname] = g.vertexTypes;
    expect(person!.index).toBe('_graphArVertexIndex');
    expect(person!.properties.map((p) => [p.name, p.type, p.primary, p.nullable, p.list])).toEqual([
      ['id', 'BIGINT', true, false, false],
      ['name', 'VARCHAR', false, false, false],
      ['age', 'INTEGER', false, true, false],
      ['born', 'DATE', false, true, false],
      ['active', 'BOOLEAN', false, true, false],
    ]);
    // Several chunks: the distinct count is a range; one chunk: exact.
    expect(person!.properties[2]!.stats).toEqual({ nulls: 1, min: '19', max: '52', distinct: { atLeast: 3, atMost: 6 } });
    expect(nickname!.properties[1]!.stats).toEqual({ nulls: 0, min: 'an', max: 'dee', distinct: { exact: 3 } });
    // A count the chunks do not carry is absent, not guessed.
    expect(software!.properties[3]!.stats).toEqual({ nulls: 1, min: '1970-01-01 00:00:00', max: '2023-11-14 22:13:20.123' });
    expect(g.edgeTypes[0]!.properties.map((p) => [p.name, p.type])).toEqual([['weight', 'DOUBLE']]);
    expect([g.edgeTypes[0]!.src, g.edgeTypes[0]!.dst]).toEqual(['_graphArSrcIndex', '_graphArDstIndex']);
  });

  it('reads one vertex, and no vertex past the count', async () => {
    const ana = await g.vertex('person', 0);
    expect(Object.keys(ana!)).toEqual(['id', 'name', 'age', 'born', 'active']);
    expect([Number(ana!['id']), ana!['name'], ana!['age']]).toEqual([100, 'ana', 31]);
    expect(await g.vertex('person', 7)).toBeNull();
    await expect(g.vertex('robot', 0)).rejects.toMatchObject({ code: 'unknown' });
  });

  it('reads every edge of a type as the indices of its ends', async () => {
    const { src, dst } = await g.links('person_created_software');
    expect(src).toBeInstanceOf(Uint32Array);
    expect([...src].map((s, i) => [s, dst[i]]).sort((a, b) => a[0]! - b[0]! || a[1]! - b[1]!)).toEqual([[0, 0], [2, 1], [6, 0], [6, 2]]);
  });

  it('reads the edges at a vertex, in each direction', async () => {
    const all = await g.neighbours('person', 0);
    expect(all.map((n) => [n.edgeType, n.direction, n.type, n.index, n.properties])).toEqual([
      ['person_knows_person', 'out', 'person', 1, { weight: 0.5 }],
      ['person_knows_person', 'out', 'person', 2, { weight: 0.25 }],
      ['person_knows_person', 'out', 'person', 6, { weight: 1.5 }],
      ['person_knows_person', 'in', 'person', 1, { weight: 0.125 }],
      ['person_knows_person', 'in', 'person', 6, { weight: 3 }],
      ['person_created_software', 'out', 'software', 0, {}],
    ]);
    expect((await g.neighbours('software', 0, { direction: 'in' })).map((n) => [n.type, n.index])).toEqual([['person', 0], ['person', 6]]);
    expect(await g.neighbours('software', 0, { direction: 'out' })).toEqual([]);
  });

  it('searches the string properties, case aside', async () => {
    expect(await g.search('AN')).toEqual([
      { type: 'person', index: 0, property: 'name', value: 'ana' },
      { type: 'nickname', index: 0, property: 'value', value: 'annie' },
      { type: 'nickname', index: 1, property: 'value', value: 'an' },
    ]);
    expect(await g.search('AN', { limit: 1 })).toHaveLength(1);
    expect(await g.search("o'")).toEqual([]);
  });

  it('counts the most common values of a property', async () => {
    expect(await g.values('person', 'active')).toEqual([{ value: 'true', count: 4 }, { value: 'false', count: 2 }]);
    expect(await g.values('person_knows_person', 'weight', { limit: 1 })).toHaveLength(1);
    await expect(g.values('person', 'height')).rejects.toMatchObject({ code: 'unknown' });
  });

  describe('match', () => {
    it('reads a pattern as one query, a column per variable.property', async () => {
      const { sql, columns } = g.match([{ as: 'p', type: 'person' }, { edge: 'created' }, { as: 's', type: 'software' }], {
        columns: ['p.name', 's.name'],
      });
      expect(columns.map((c) => [c.name, c.variable, c.property?.type])).toEqual([['p.name', 'p', 'VARCHAR'], ['s.name', 's', 'VARCHAR']]);
      expect(await rows(`SELECT * FROM (${sql}) ORDER BY ALL`)).toEqual([['ana', 'lop'], ['cy', 'ripple'], ['gus', 'lop'], ['gus', 'nebula']]);
      expect(Object.keys((await e.rows(sql))[0]!)).toEqual(['p.name', 's.name']);
    });

    it('filters each element by its own columns, and reads an edge against its direction', async () => {
      const { sql } = g.match(
        [{ as: 's', type: 'software' }, { edge: 'created', direction: 'in' }, { as: 'p', type: 'person', where: "name = 'gus'" }],
        { columns: ['s.name'] },
      );
      expect(await rows(`SELECT * FROM (${sql}) ORDER BY ALL`)).toEqual([['lop'], ['nebula']]);
    });

    it('follows several hops, with edge variables', async () => {
      const { sql } = g.match(
        [{ as: 'p', type: 'person', where: "name = 'bo'" }, { as: 'k', edge: 'knows' }, { type: 'person' }, { edge: 'created' }, { as: 's', type: 'software' }],
        { columns: ['k.weight', 's.name'] },
      );
      expect(await rows(`SELECT * FROM (${sql}) ORDER BY ALL`)).toEqual([[0.125, 'lop'], [0.75, 'ripple']]);
    });

    it("answers a vertex's index for its bare variable, which a reach predicate applies to", async () => {
      const { sql, columns } = g.match([{ as: 'p', type: 'person' }, { edge: 'created' }, { as: 's', type: 'software' }], {
        columns: ['p', 's.name'],
      });
      expect(columns[0]).toEqual({ name: 'p', variable: 'p' });
      const lop = g.reach({ type: 'software', where: "name = 'lop'" }, 'person').replace('"_graphArVertexIndex"', '"p"');
      expect(await rows(`SELECT DISTINCT p::INTEGER FROM (${sql}) WHERE ${lop} ORDER BY ALL`)).toEqual([[0], [6]]);
      expect(() => g.match([{ as: 'p', type: 'person' }, { as: 'k', edge: 'knows' }, { type: 'person' }], { columns: ['k'] })).toThrow(GraphArError);
    });

    it('reads every property of every named element without columns', () => {
      const { columns } = g.match([{ as: 's', type: 'software' }]);
      expect(columns.map((c) => c.name)).toEqual(['s.id', 's.name', 's.price', 's.released']);
    });

    it('refuses what the graph does not hold, and what is not a pattern', () => {
      expect(() => g.match([{ type: 'person' }, { edge: 'created' }, { type: 'person' }])).toThrow(GraphArError);
      expect(() => g.match([{ as: 'p', type: 'person' }], { columns: ['p.height'] })).toThrow(/no property `height`/);
      expect(() => g.match([{ type: 'person' }, { edge: 'knows' }])).toThrow(TypeError);
      expect(() => g.match([{ as: 'p', type: 'person' }, { edge: 'knows' }, { as: 'p', type: 'person' }])).toThrow(TypeError);
    });
  });

  describe('reach', () => {
    const where = async (type: string, predicate: string) => rows(`SELECT name FROM "g".${JSON.stringify(type)} WHERE ${predicate} ORDER BY ALL`);

    it('carries a filter on one type to another along the one path between them', async () => {
      expect(await where('person', g.reach({ type: 'software', where: "name = 'lop'" }, 'person'))).toEqual([['ana'], ['gus']]);
      expect(await where('software', g.reach({ type: 'person', where: "name = 'gus'" }, 'software'))).toEqual([['lop'], ['nebula']]);
    });

    it('is the filter itself on its own type', () => {
      expect(g.reach({ type: 'person', where: 'age > 30' }, 'person')).toBe('(age > 30)');
    });

    it('follows the path via names', async () => {
      const via = [{ type: 'person' }, { edge: 'knows' }, { type: 'person' }, { edge: 'created' }, { type: 'software' }] as const;
      expect(await where('software', g.reach({ type: 'person', where: "name = 'bo'" }, 'software', { via }))).toEqual([['lop'], ['ripple']]);
      expect(() => g.reach({ type: 'person', where: 'true' }, 'person', { via })).toThrow(TypeError);
    });

    it('refuses types no path joins', () => {
      expect(() => g.reach({ type: 'nickname', where: 'true' }, 'person')).toThrow(/no path joins/);
    });
  });
});

describe('reach between types two paths join', () => {
  const vertex = (type: string): VertexType => ({ type, count: 1, table: `"x".${JSON.stringify(type)}`, index: 'i', properties: [] });
  const edge = (srcType: string, edgeType: string, dstType: string): EdgeType => ({
    name: `${srcType}_${edgeType}_${dstType}`, edgeType, srcType, dstType, count: 1, directed: true,
    table: `"x"."${srcType}_${edgeType}_${dstType}"`, src: 's', dst: 'd', properties: [],
  });
  const g = graphOf({
    engine: { query: () => Promise.reject(new Error('no engine')) },
    vertexTypes: [vertex('person'), vertex('university')],
    edgeTypes: [edge('person', 'studied', 'university'), edge('person', 'taught', 'university')],
    detach: async () => {},
  });

  it('asks for the path instead of choosing one', () => {
    expect(() => g.reach({ type: 'person', where: 'true' }, 'university')).toThrow(
      expect.objectContaining({ code: 'ambiguous', message: expect.stringContaining('2 shortest paths') }),
    );
    expect(g.reach({ type: 'person', where: 'true' }, 'university', {
      via: [{ type: 'person' }, { edge: 'taught' }, { type: 'university' }],
    })).toContain('"x"."person_taught_university"');
  });
});
