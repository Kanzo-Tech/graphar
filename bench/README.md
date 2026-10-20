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

The same triples are written five ways:

| layout | what | written by |
| --- | --- | --- |
| `nt` | N-Triples | `oxttl` |
| `nt_gz` | N-Triples, gzip default level | `flate2` |
| `hdt` | HDT | the `hdt` crate, from the N-Triples |
| `spo` | one Parquet table, a row per triple: `subject, predicate, object, datatype, lang` as text, sorted by subject | `parquet`, zstd |
| `gar` | GraphAr through `graphar-rdf`, chunks of 2^16 rows: a table per class, edges, `mapping.ttl`, the counts in every footer and a Puffin file per type of several chunks | `graphar-rdf`, zstd |

and for each: `size_bytes`; `write_seconds` (GAR with and without `graphar-stats`, and
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

SPO and GAR are read by DuckDB, GAR through one view that is the mapping's triples maps as a
`UNION ALL` (the date range asks the `Student` table's typed column instead); HDT by its triple
patterns. Every layout must return the same count for every access, and the whole graph from each
must be the triples written, or the run fails.

Every time is the median of `--runs` runs after one unmeasured run, on local files. Access over
object storage is not measured.
