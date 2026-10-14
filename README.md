# graphar

Rust crates for [Apache GraphAr](https://graphar.apache.org), the file format for property graphs:
a graph as Parquet (or ORC, CSV, JSON) chunks, described by YAML information files. They implement
the [format specification](https://graphar.apache.org/docs/specification/format), and every crate
builds for `wasm32-unknown-unknown`.

| crate | what it is |
| --- | --- |
| [`graphar-info`](crates/graphar-info) | The information files: `GraphInfo`, `VertexInfo`, `EdgeInfo` and their parts, loaded from and dumped to YAML, and validated. No IO of its own |

It is tested against every information file of
[incubator-graphar-testing](https://github.com/apache/incubator-graphar-testing).

```rust
let graph = graphar_info::GraphInfo::load_file("ldbc_sample.graph.yml")?;
graph.validate()?;
for (file, yaml) in graph.dump()? { /* write it */ }
```

Licensed under the [Apache License 2.0](LICENSE).
