/** A native DuckDB connection as an `Engine`, the way a host adapts its own. */

import { DuckDBInstance } from '@duckdb/node-api';

import type { Engine, Table } from '../src/index.js';

export interface TestEngine extends Engine {
  /** Every statement run, in order. */
  readonly statements: string[];
  rows(sql: string): Promise<Record<string, unknown>[]>;
}

export async function engine(): Promise<TestEngine> {
  const db = await DuckDBInstance.create(':memory:');
  const connection = await db.connect();
  const statements: string[] = [];
  const query = async (sql: string, { signal }: { readonly signal: AbortSignal }): Promise<Table> => {
    signal.throwIfAborted();
    statements.push(sql);
    const reader = await connection.runAndReadAll(sql);
    signal.throwIfAborted();
    const columns = reader.getColumnsObjectJS() as Record<string, unknown[]>;
    return {
      numRows: reader.currentRowCount,
      getChild: (name) => (name in columns ? { get: (i: number) => columns[name]![i] } : null),
    };
  };
  return {
    statements,
    query,
    async rows(sql) {
      const reader = await connection.runAndReadAll(sql);
      return reader.getRowObjectsJS() as Record<string, unknown>[];
    },
  };
}
