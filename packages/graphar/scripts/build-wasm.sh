#!/usr/bin/env bash
# Builds packages/graphar/pkg/ from bindings/graphar-rdf: the module `@kanzo-tech/graphar/write`
# loads, and the only one the package carries. pkg/ is gitignored and in the published tarball.
#
# Needs the wasm32-unknown-unknown target and wasm-bindgen-cli at the exact version of the
# `wasm-bindgen` crate in the root Cargo.toml.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
WANT=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' Cargo.toml)
HAVE=$(wasm-bindgen --version | awk '{print $2}')
if [[ "$HAVE" != "$WANT" ]]; then
  echo "wasm-bindgen CLI is $HAVE; the crate is $WANT: cargo install --locked --version $WANT wasm-bindgen-cli" >&2
  exit 1
fi
cargo build --locked --profile wasm-release --target wasm32-unknown-unknown -p graphar-rdf-wasm
# --target web: the glue finds its .wasm by `new URL(…, import.meta.url)`, which a bundler emits as
# an asset; Node, whose `fetch` refuses `file:`, hands `initWriter` the bytes.
wasm-bindgen --target web --out-dir packages/graphar/pkg \
  target/wasm32-unknown-unknown/wasm-release/graphar_rdf_wasm.wasm
