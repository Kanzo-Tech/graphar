/**
 * What {@link GraphArError.code} says went wrong:
 * - `unreadable`: a file did not read through the engine;
 * - `unparseable`: a document is not YAML;
 * - `invalid`: a document is not GraphAr's — a key it does not define, a missing one, a version
 *   other than `gar/v1` — or a count file is not one `int64`;
 * - `unsupported`: a file type other than `parquet`;
 * - `duplicate`: two relations of the graph would have one name;
 * - `engine`: the engine refused a statement;
 * - `unknown`: a read or a pattern names a type, an edge type or a property the graph does not
 *   hold, or asks for a path between two types that no path joins;
 * - `ambiguous`: more than one shortest path joins two types, and nothing names one.
 */
export type GraphArErrorCode = 'unreadable' | 'unparseable' | 'invalid' | 'unsupported' | 'duplicate' | 'engine' | 'unknown' | 'ambiguous';

/** Everything `attachGraph` and a `Graph` throw but an abort, which rejects with the signal's reason. */
export class GraphArError extends Error {
  override readonly name = 'GraphArError';
  /** The document or file it is about, when there is one. */
  readonly document: string | undefined;

  constructor(
    readonly code: GraphArErrorCode,
    message: string,
    options: { document?: string; cause?: unknown } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.document = options.document;
  }
}
