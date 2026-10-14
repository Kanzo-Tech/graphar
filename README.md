# graphar

Rust crates for [Apache GraphAr](https://graphar.apache.org), the file format for property graphs:
a graph as Parquet (or ORC, CSV, JSON) chunks, described by YAML information files. They implement
the [format specification](https://graphar.apache.org/docs/specification/format), and every crate
builds for `wasm32-unknown-unknown`.

| crate | what it is |
| --- | --- |
| [`graphar-info`](crates/graphar-info) | The information files: `GraphInfo`, `VertexInfo`, `EdgeInfo` and their parts, loaded from and dumped to YAML, and validated. No IO of its own |
| [`graphar-parquet`](crates/graphar-parquet) | The data files in Parquet: vertex chunks, adjacency lists, offsets, edge properties and the count files, written and read through any [`object_store`](https://docs.rs/object_store). No runtime: its wasm32 build reaches no `tokio` |
| [`@kanzo-tech/graphar-duckdb`](packages/graphar-duckdb) (npm) | `attachGraph`: a graph on a host's DuckDB as a catalog, a view per vertex and edge type over the Parquet chunks, and the information files as `graphar_vertex_types`, `graphar_edge_types`, `graphar_properties` and `graphar_extra_info`. TypeScript only: every file is read through the engine |

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

## Releasing

A release is a GitHub Release, and its body is the release notes: there is no CHANGELOG. The tag
decides the version and all three artifacts share it (`graphar-info`, `graphar-parquet`,
`@kanzo-tech/graphar-duckdb`).

1. In one commit, set `version` in both `crates/*/Cargo.toml` and in `packages/*/package.json` (and
   `graphar-info`'s version in `graphar-parquet`'s dependency in the root `Cargo.toml`), then merge it.
2. Publish a GitHub Release tagged `v<version>` on that commit (`v0.1.0-alpha.0`; a prerelease tag
   publishes to npm under its identifier as the dist-tag).
3. `.github/workflows/release.yml` checks the tag against every manifest, runs the whole CI chain on
   the commit, then publishes `graphar-info`, `graphar-parquet` and the npm package.

The repository needs two secrets, `CRATES_IO_TOKEN` (crates.io, publish scopes) and `NPM_TOKEN` (an
npm automation token for `@kanzo-tech`). npm packages are published with provenance.

Licensed under the [Apache License 2.0](LICENSE).
