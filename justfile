# Development and release tasks for attricat-stock-valuation.
#
# Attricat installs a zstd-compressed tar archive whose root contains the
# manifest and every artifact path the manifest declares.

extension_id := "attricat-stock-valuation"
version := `node -p "require('./manifest.json').version"`
dist_dir := "dist"
stage_dir := "dist/package"
archive := dist_dir / extension_id + "-" + version + ".tar.zst"

default: check

fmt:
  cargo fmt --check

test:
  cargo test --workspace

clippy:
  cargo clippy --workspace --all-targets -- -D warnings

check: fmt clippy test

# Build a component that imports only the attricat:host@1.1.0 ABI.
# wasm32-wasip2 would import ambient WASI interfaces, which Attricat never links.
build-server:
  #!/usr/bin/env bash
  set -euo pipefail
  command -v wasm-tools >/dev/null || { echo "Install wasm-tools: cargo install wasm-tools --locked" >&2; exit 1; }
  rustup target add wasm32-unknown-unknown
  cargo build --release --target wasm32-unknown-unknown -p attricat-stock-valuation-server
  mkdir -p {{dist_dir}}
  wasm-tools component new target/wasm32-unknown-unknown/release/attricat_stock_valuation_server.wasm -o {{dist_dir}}/server.wasm
  if wasm-tools component wit {{dist_dir}}/server.wasm | grep -E '^\s*import ' | grep -v 'attricat:host/.*@1\.1\.0'; then
    echo "server.wasm imports something other than attricat:host@1.1.0" >&2; exit 1
  fi

build: check build-server

# The archive Attricat's installer accepts: the manifest at the root plus
# exactly the files it declares, the README and the icon.
pack: build
  #!/usr/bin/env bash
  set -euo pipefail
  rm -rf {{stage_dir}}
  mkdir -p {{stage_dir}}/dist
  cp manifest.json README.md {{stage_dir}}/
  cp -R assets {{stage_dir}}/
  cp {{dist_dir}}/server.wasm {{stage_dir}}/dist/
  rm -f {{archive}}
  (cd {{stage_dir}} && tar -cf - manifest.json README.md assets dist/server.wasm) | zstd -q -o {{archive}}
  echo "Created {{archive}}"

clean:
  rm -rf {{dist_dir}} target
