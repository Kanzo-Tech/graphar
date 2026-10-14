# graphar — rules, not context

- **The reference is Apache GraphAr**: the
  [format specification](https://graphar.apache.org/docs/specification/format) first; where it is
  silent, the C++ library (`cpp/src/graphar/graph_info.cc`) and the files of
  `apache/incubator-graphar-testing`. Where our code departs from C++, the comment beside it says so
  and cites the specification. Names are GraphAr's, so the code can go upstream without a rename.
- **No legacy, no shims, no aliases.** A key GraphAr no longer defines is refused, not read through
  an alias and not dropped (`deny_unknown_fields`). Rename and delete outright.
- **Minimal and generic.** One way to express each thing; grep for the existing one before adding.
- **Every crate builds for `wasm32-unknown-unknown`**: no `tokio`, no network, no IO beyond a thin
  file-system convenience. The model is load/dump over `&str`. CI checks it.
- **YAML is `serde_yaml_ng`.** `serde_yml` is banned (RUSTSEC-2025-0068).
- `tests/fixtures/` are vendored unchanged; their source commit is in the README there. Do not edit
  them. A fixture we refuse is listed with the reason in the test that refuses it.
- Before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `cargo check --target wasm32-unknown-unknown -p graphar-info`.
