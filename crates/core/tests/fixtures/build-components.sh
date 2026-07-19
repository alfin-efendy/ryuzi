#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
FIXTURES="$ROOT/crates/core/tests/fixtures"
TARGET=wasm32-wasip2

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "missing Rust target $TARGET; install it with: rustup target add $TARGET" >&2
  exit 1
fi

materialize_deps() {
  fixture=$1
  deps="$fixture/wit/deps"
  rm -rf "$deps"
  mkdir -p "$deps"
  # wit-bindgen 0.57 cannot parse the named imports in the production world,
  # but its types/lifecycle interfaces remain the canonical contract.
  awk '
    /^world plugin[[:space:]]*\{/ { skipping=1; depth=1; next }
    skipping {
      opens=gsub(/\{/, "{"); closes=gsub(/\}/, "}"); depth += opens - closes
      if (depth == 0) skipping=0
      next
    }
    { print }
  ' "$ROOT/crates/plugin-sdk/wit/plugin.wit" > "$deps/plugin.wit"
  cp "$ROOT"/crates/plugin-sdk/wit/deps/*.wit "$deps/"
}

build_fixture() {
  fixture="$FIXTURES/$1"
  materialize_deps "$fixture"
  cargo build --manifest-path "$fixture/Cargo.toml" --target "$TARGET" --release
  artifact="$fixture/target/$TARGET/release/$2.wasm"
  test -f "$artifact"
  printf '%s\n' "$artifact"
}

build_fixture component-noop ryuzi_component_noop_fixture
build_fixture component-http-import ryuzi_component_http_fixture
build_fixture component-connector ryuzi_component_connector_fixture
build_fixture component-hooks ryuzi_component_hooks_fixture
