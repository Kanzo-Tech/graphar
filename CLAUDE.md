# graphar — rules, not context

- **The reference is Apache GraphAr**: the
  [format specification](https://graphar.apache.org/docs/specification/format) first; where it is
  silent, the C++ library (`cpp/src/graphar/graph_info.cc`) and the files of
  `apache/incubator-graphar-testing`. Where our code departs from C++, the comment beside it says so
  and cites the specification. Names are GraphAr's, so the code can go upstream without a rename.
- **No legacy, no shims, no aliases.** A key GraphAr no longer defines is refused, not read through
  an alias and not dropped (`deny_unknown_fields`). Rename and delete outright.
- **Minimal and generic.** One way to express each thing; grep for the existing one before adding.
- **Every crate builds for `wasm32-unknown-unknown`**: no `tokio` in its normal dependencies, no
  network, and no IO but a thin file-system convenience or the caller's `object_store`. The model is
  load/dump over `&str`. CI checks both.
- **YAML is `serde_yaml_ng`.** `serde_yml` is banned (RUSTSEC-2025-0068).
- `tests/fixtures/` are vendored unchanged; their source commit is in the README there. Do not edit
  them. A fixture we refuse is listed with the reason in the test that refuses it.
- `conformance/sample/` and `conformance/user_defined/` are written by `cargo run -p graphar-parquet
  --example sample -- conformance`, never by hand; CI rewrites them and fails on a difference.
- **Formal correctness.** Where the specification is explicit, follow it to the letter; where it is
  silent, follow the C++ library and say so. Every departure carries a comment quoting the exact
  specification sentence, and anything non-standard (a user-defined type's Arrow type) is declared
  through the format's own extension point (the version's `gar/v1 (name)`) and the caller, never
  assumed.
- Before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`,
  `cargo check --target wasm32-unknown-unknown -p graphar-info -p graphar-parquet -p graphar-rdf`, and
  `pnpm install && pnpm typecheck && pnpm test && pnpm build`.
- `@kanzo-tech/graphar-duckdb` links no engine and loads no WASM: every file it reads goes through
  the host's DuckDB, by the name the host's `resolve` gives it.
- Releases are GitHub Releases (`.github/workflows/release.yml`, see the README): the tag `v<version>`
  must equal every crate's and package's `version`, the release body is the release notes, and there
  is no CHANGELOG. It needs the secrets `CRATES_IO_TOKEN` and `NPM_TOKEN`. Never publish by hand.
