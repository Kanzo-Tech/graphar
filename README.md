# graphar

Rust crates for [Apache GraphAr](https://graphar.apache.org), the file format for property graphs:
a graph as Parquet (or ORC, CSV, JSON) chunks, described by YAML information files. They implement
the [format specification](https://graphar.apache.org/docs/specification/format), and every crate
builds for `wasm32-unknown-unknown`.

| crate | what it is |
| --- | --- |
| [`graphar-info`](crates/graphar-info) | The information files: `GraphInfo`, `VertexInfo`, `EdgeInfo` and their parts, loaded from and dumped to YAML, and validated. No IO of its own |
| [`graphar-parquet`](crates/graphar-parquet) | The data files in Parquet: vertex chunks, adjacency lists, offsets, edge properties and the count files, written and read through any [`object_store`](https://docs.rs/object_store), each chunk one row group whose Parquet `Statistics` carry its distinct counts. No runtime: its wasm32 build reaches no `tokio` |
| [`graphar-rdf`](crates/graphar-rdf) | RDF in GraphAr: an RDF graph and its shapes (SHACL Core in Turtle, ShEx in ShExC or ShExJ, read by [rudof](https://github.com/rudof-project/rudof)) written as a vertex table per shape (the nodes that validate against it) and an edge type per property between shapes, with the RDF reading of those tables as a plan in the mapping algebra of Min Oo and Hartig, written as RML-Core (`mapping.ttl`) and R2RML (`mapping.r2rml.ttl`). Only a graph that conforms to the closed reading of its shapes is written; `read` gives back the triples written, up to canonical lexical form |
| [`@kanzo-tech/graphar-duckdb`](packages/graphar-duckdb) (npm) | `attachGraph`: a graph on a host's DuckDB, read through a `Graph` — its types and their properties with each one's statistics, a vertex, the edges of a type, the edges at a vertex, search, common values, and patterns across types as SQL (`match`, and `reach`, a filter on one type carried to another) — over a catalog of a view per vertex and edge type over the Parquet chunks, and the information files as `graphar_vertex_types`, `graphar_edge_types`, `graphar_properties`, `graphar_extra_info` and `graphar_chunks` (every chunk file, by the name the host resolved it to), and `graphar_column_stats`, each property's nulls, bounds and distinct count combined from its chunks' Parquet `Statistics`. TypeScript only: every file is read through the engine |

They are tested against every information file and the Parquet data of
[incubator-graphar-testing](https://github.com/apache/incubator-graphar-testing), and
[`conformance/`](conformance) holds a graph `graphar-parquet` wrote, which CI opens with GraphAr's
own C++ library (`conformance/read_cpp.py`).

```rust
let graph = graphar_info::GraphInfo::load_file("ldbc_sample.graph.yml")?;
graph.validate()?;
for (file, yaml) in graph.dump()? { /* write it */ }

// a vertex type's table, then the graph's documents, the graph document last
graphar_parquet::write_vertices(&store, &root, &graph.vertices[0], &batches, &props).await?;
graphar_parquet::write_info(&store, &root, &graph).await?;
```

## Where this departs from GraphAr's C++ library

The specification comes first, C++ where it is silent, and each departure is commented at the code
with the sentence it follows. The one that matters to a reader is **user-defined types**. The
specification says: "GraphAr also supports the user-defined data types, which can be used to
represent complex data structures, such as the struct, map, and union types" (format.md,
"Property Data Types"), and C++ implements the declaration (`gar/v1 (langString)`) but not the type:
its `DataType::TypeNameToDataType` throws "Unsupported data type" on any name outside a fixed list,
so `GraphInfo::Load` fails for the whole graph. It fails the same way for `time`, which the
specification lists. These crates follow the specification:

- `graphar-info` reads and writes `data_type: langString` with `cardinality: set` and requires the
  version to declare `langString`. The list is the cardinality's: C++'s writer wraps every property
  that is not `single` in an Arrow list, so also writing `list<…>` would wrap it twice.
- `graphar-parquet` takes the Arrow type of each user-defined type from the caller
  (`UserTypes`), because the specification defines none; a column must match it exactly. `langString`
  as `STRUCT<value: STRING, lang: STRING>` is stored as Parquet `LIST<STRUCT<value, lang>>`.
- `graphar-duckdb` reads the Parquet types as they are (a DuckDB `STRUCT(value VARCHAR, lang
  VARCHAR)[]`), and needs `userTypes` only to type a view that has no rows.

`graphar-info` still carries `time`, which the specification lists; a writer that wants a reader
to open the graph writes such a value as `string`, since neither C++ nor Java accepts `time`.

`conformance/user_defined/` is such a graph and `conformance/read_cpp.py` records what C++ does with
it: it refuses it, and the check fails the day C++ opens it. `conformance/sample/`, which C++ opens
and reads, has no such column.

## RDF

`graphar-rdf` writes an RDF graph that conforms to the closed reading of its shapes (`sh:closed`,
ShEx `CLOSED`): every subject an IRI that validates against exactly one shape, and every triple a
value of one of that shape's properties or an edge to a node of the shape it names, with
`sh:maxCount 1` respected. A node's type is the shape it validates against, not its `rdf:type`,
which is a predicate like any other; a shape that fixes it to one class has it written as the
triples map's `rml:class`. The typing is rudof's validator's maximal valid shape map
(`Typing::Validate`), or one the caller states and vouches for (`Typing::Assume`). Any other graph
is refused before a file is written, with the subject and the validator's reasons, or the first
triple that has no place and why. A literal is stored as its value, so `"031"^^xsd:integer` reads
back as `"31"^^xsd:integer`. The reading is one plan in the mapping algebra of Min Oo and Hartig
(ESWC 2025), which `read` evaluates and which is written twice: as RML-Core, with the language of a
tagged string read per row, and as R2RML, whose language is a constant, with a triples map per
language the column holds. Both delimit every SQL identifier and write standard SQL, and
[`conformance/rml/run.py`](conformance/rml/run.py) runs both in CI through Morph-KGC on DuckDB and
PostgreSQL and RMLMapper on PostgreSQL, against the triples written. One defect of RMLMapper 8.1.0
is explained rather than hidden there: booleans read from PostgreSQL as `t`/`f`. And
[`conformance/rml/algemaploom.py`](conformance/rml/algemaploom.py) checks that AlgeMapLoom
(RMLio/mappingloom-rs) translates `mapping.ttl` into the same plan: a pipeline per triples map, over
its source, writing its triples.

Mapping and shapes are each their own file. The mapping is RML-Core (and R2RML) and says only how
each table and column becomes triples; each vertex type's triples map adds one statement,
`dct:conformsTo` its shape, which generates no triple and is what the typing guaranteed of every
row. Under `Typing::Validate` the writer saves the shape document beside the graph exactly as it
read it, as `shapes.ttl` (SHACL in Turtle), `shapes.shex` (ShExC) or `shapes.shexj` (ShExJ), and
the graph document's `extra_info` names it under the key `shapes`; under `Typing::Assume` there is
none. Labels and descriptions are the shapes' (`sh:name`, `sh:description`; in ShExJ, `rdfs:label`
and `rdfs:comment` annotations) and are read from there: without shapes there are no labels, and a
consumer shows an IRI's local name. A triples map is named by the GraphAr names it reads:
`<#Person>`, `<#Person/nick>`, `<#Person_knows_Person>`.

## Releasing

A release is a GitHub Release, and its body is the release notes: there is no CHANGELOG. The tag
decides the version and every artifact shares it (`graphar-info`, `graphar-parquet`,
`graphar-rdf`, `@kanzo-tech/graphar-duckdb`).

1. In one commit, set `version` in every `crates/*/Cargo.toml` and in `packages/*/package.json` (and
   the versions of the path dependencies in the root `Cargo.toml`), then merge it.
2. Publish a GitHub Release tagged `v<version>` on that commit (`v0.1.0-alpha.0`; a prerelease tag
   publishes to npm under its identifier as the dist-tag).
3. `.github/workflows/release.yml` checks the tag against every manifest, runs the whole CI chain on
   the commit, then publishes `graphar-info`, `graphar-parquet` and the npm package.

The repository needs two secrets, `CRATES_IO_TOKEN` (crates.io, publish scopes) and `NPM_TOKEN` (an
npm automation token for `@kanzo-tech`). npm packages are published with provenance.

Licensed under the [Apache License 2.0](LICENSE).
