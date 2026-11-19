/**
 * `@kanzo-tech/graphar/write`: a graph written as GraphAr, through the host's `put`. The one entry
 * of the package that loads WebAssembly — `graphar-rdf`'s writer, built for wasm32 — so `attach`
 * and everything else stay free of it.
 */

import type { Quad } from '@rdfjs/types';
import { Writer } from 'n3';

// '../pkg/graphar_rdf_wasm.js' is wasm-bindgen's `--target web` glue, which `build:wasm` makes:
// gitignored, and in the published tarball.
import init, { type InitInput, writeRdf } from '../pkg/graphar_rdf_wasm.js';

import type { attach } from './attach.js';
import type { Graph } from './graph.js';
import type { Shapes } from './rdf.js';

export type { InitInput };

/**
 * Where the graph goes: the host puts `bytes` at `path`, relative to the graph's root, and
 * resolves once it is stored. Called once per file, in the order the writer finishes them, the
 * graph document last.
 */
export type Put = (path: string, bytes: Uint8Array, options: { readonly signal: AbortSignal }) => Promise<void>;

/**
 * An RDF graph to write: its shape document and its triples, **grouped by subject** — each
 * subject's triples together, in any order of subjects, as a Turtle document and a mapping engine
 * give them. A subject that comes back after another's triples is refused.
 */
export interface RdfSource {
  readonly rdf: {
    readonly shapes: Shapes;
    readonly triples: AsyncIterable<Quad> | Iterable<Quad>;
  };
}

/** How {@link write} writes. */
export interface WriteOptions {
  /** The graph's name: `<name>.graph.yml`. `graph` by default. */
  readonly name?: string;
  /** The rows of each chunk: 2²⁰ by default. */
  readonly chunkSize?: number;
  readonly signal?: AbortSignal;
}

/** What {@link write} wrote. */
export interface Written {
  /** The rows of each vertex type, in the graph's order. */
  readonly vertices: readonly (readonly [string, number])[];
  /** The edges of each edge type, in the graph's order. */
  readonly edges: readonly (readonly [string, number])[];
  /** The distinct triples written. */
  readonly triples: number;
}

/** The module's file, beside its glue. */
const MODULE = new URL('../pkg/graphar_rdf_wasm_bg.wasm', import.meta.url);

let booted: Promise<unknown> | null = null;

/**
 * Loads the writer's module ahead of the first {@link write}, which loads it itself otherwise. A
 * bundler emits the module as an asset and the browser fetches it; Node, whose `fetch` refuses a
 * `file:` URL, hands the bytes here first. A load that failed is forgotten, so the next tries again.
 */
export function initWriter(wasm?: InitInput): Promise<unknown> {
  if (booted === null) {
    const pending = init({ module_or_path: wasm ?? MODULE });
    booted = pending;
    pending.catch(() => {
      if (booted === pending) booted = null;
    });
  }
  return booted;
}

/** Triples to N-Triples, a batch of lines at a time: what the module parses. */
async function* lines(triples: AsyncIterable<Quad> | Iterable<Quad>, signal: AbortSignal | undefined): AsyncIterator<string> {
  const BATCH = 4096;
  const writer = new Writer({ format: 'N-Triples' });
  let batch: Quad[] = [];
  for await (const q of triples) {
    batch.push(q);
    if (batch.length === BATCH) {
      signal?.throwIfAborted();
      yield writer.quadsToString(batch);
      batch = [];
    }
  }
  signal?.throwIfAborted();
  if (batch.length > 0) yield writer.quadsToString(batch);
}

/**
 * **Writes `source` as an Apache GraphAr graph** through `put`. For RDF, `graphar-rdf`'s writer:
 * a vertex type per shape and an edge type per property between shapes, each subject the type of
 * the shape that fixes its class, the mapping `mapping.ttl` (and `mapping.r2rml.ttl`) and the shape
 * document beside the tables, and the graph document last, naming both in `extra_info`, so
 * {@link attach} reads it back with {@link Graph.rdf}. It streams: a chunk is put as soon as it
 * fills, and what it holds is every subject's IRI, a chunk per type and one subject's triples.
 *
 * A graph that does not conform to the closed reading of its shapes is refused, naming the subject
 * or the triple, and its graph document is never put: what was put before is left, uncommitted.
 *
 * @throws {Error} the writer's refusal; a rejection of `put` or of the triples, as it came. An
 *   abort rejects with the signal's reason.
 */
export async function write(put: Put, source: RdfSource, options: WriteOptions = {}): Promise<Written> {
  const { name = 'graph', chunkSize = 1 << 20, signal } = options;
  if (!Number.isSafeInteger(chunkSize) || chunkSize < 1) throw new RangeError(`chunk size ${chunkSize} is not a count of rows`);
  await initWriter();
  signal?.throwIfAborted();
  const quiet = new AbortController().signal;
  const putting = (path: string, bytes: Uint8Array) => {
    signal?.throwIfAborted();
    return put(path, bytes, { signal: signal ?? quiet });
  };
  const { shapes, triples } = source.rdf;
  try {
    return (await writeRdf(putting, shapes.syntax, shapes.text, lines(triples, signal), name, chunkSize)) as Written;
  } catch (cause) {
    signal?.throwIfAborted();
    throw cause;
  }
}
