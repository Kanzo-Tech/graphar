# graphar-bench

The measurements of *Leveraging Parquet for representing RDF data*. One command writes
[`results.json`](results.json), and the paper's `numbers.tex` copies every measured figure from
that file and from nowhere else:

```sh
cargo run --release -p graphar-bench -- --scales 1,10,60 --runs 5
```

`results.json` records the commit it ran at (and whether the tree was dirty), the host, the number
of runs and the versions of the engines.

## What it measures

The dataset is synthetic (`src/generate.rs`): universities in the shape of LUBM's, with the typed
literals LUBM lacks (integers, dates, language-tagged labels), and its SHACL shapes, to which it
conforms read closed, as graphar-rdf requires. A scale is a number of universities, about 17,000
triples each.

The same triples are written six ways:

| layout | what | written by |
| --- | --- | --- |
| `nt` | N-Triples | `oxttl` |
| `nt_gz` | N-Triples, gzip default level | `flate2` |
| `hdt` | HDT | the `hdt` crate, from the N-Triples |
| `spo` | one Parquet table, a row per triple: `subject, predicate, object, datatype, lang` as text, sorted by subject | `parquet`, zstd |
| `zarr` | the Zarr layout of the earlier Zarr paper, as [ZarrDF](https://github.com/weso/ZarrDF) writes it: three term dictionaries in the root group's attributes, and `subject`, `predicate` and `object` arrays of id pairs with one chunk per term, gzip level 5 (`src/zarr.rs`) | `zarrs` |
| `gar` | GraphAr through `graphar-rdf`, chunks of 2^16 rows: a table per class, edges, `mapping.ttl`, the counts in every footer and a Puffin file per type of several chunks | `graphar-rdf`, zstd |

and for each: `size_bytes` (and the number of files of GAR and Zarr); `write_seconds` (GAR with and without `graphar-stats`, and
`stats_share`, the share of the write spent counting); `load_seconds` (a full parse of the
N-Triples, and opening the HDT file); and `access_seconds` for six accesses, with `access_rows`,
the number of triples (or subjects, for the date range) each returns:

| access | asks |
| --- | --- |
| `subject` | every triple of one professor |
| `predicate` | every `ub:takesCourse` triple |
| `predicate_object` | `ub:name "Course 5"` |
| `date_range` | students enrolled in 2015 |
| `out_neighbourhood` | the courses one student takes |
| `in_neighbourhood` | the students who take one course |

On local files (`access_seconds`), SPO and GAR are read by DuckDB, GAR through the mapping's
triples maps as a `UNION ALL`, all of them (`gar_view`) or only those that can hold the bound
predicate (`gar`; the date range asks the `Student` table's typed column instead); HDT and Zarr by
their triple patterns. Every time is the median of `--runs` runs after one unmeasured run.

Over object storage (`access_remote`), each access is read cold, by a reader with nothing but the
bucket, and counted: `requests` (GETs, whole or a byte range, HEADs and LISTs) and the `bytes` the
GETs return. Every layout is read through an `object_store` that counts each call, which is one
HTTP request to an S3-compatible bucket. SPO and GAR are read by DataFusion with filter pushdown
and the page index, registering only the tables the query names and each from its files by name,
never by listing: GAR's chunks are named by the graph document, the table's information file and
its count files (a GET each), as GraphAr's path conventions derive them; Zarr by its pattern reader, which
first reads the group (and so all three dictionaries); HDT and gzipped N-Triples have no partial
access, and are one GET of the whole file.

Every layout must return the same count for every access, locally and remotely, and the whole
graph from SPO and GAR must be the triples written, or the run fails.

ZarrDF itself is not run: at its only commit (`f4652c4`) it chunks the triples in input order
rather than by term id, stores the subject and predicate ids in every array, and offsets the rows
of every term past the 10,000th, so a lookup returns another term's triples or `ChunkNotFound`.
`src/zarr.rs` writes the layout it describes with those three corrected.
