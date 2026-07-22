#!/usr/bin/env sh
set -eu

# Build the first-party GitHub connector component (plugins/github) to
# wasm32-wasip2, materializing its wit/deps the same way the fixture builder
# (build-components.sh) and the release signer (scripts/plugins/build-first-party.ts)
# do. `plugins/github` is a STANDALONE workspace crate (not a tests/fixtures
# fixture), so this is a sibling of build-components.sh rather than another
# `build_fixture` line. It touches only `plugins/github/wit/deps` (gitignored),
# so it never races build-components.sh's `wit/deps` rewrites of the fixtures.

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
PLUGIN="$ROOT/plugins/github"
SDK_WIT="$ROOT/crates/plugin-sdk/wit"
TARGET=wasm32-wasip2

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "missing Rust target $TARGET; install it with: rustup target add $TARGET" >&2
  exit 1
fi

deps="$PLUGIN/wit/deps"
rm -rf "$deps"
mkdir -p "$deps"
# wit-bindgen 0.57 cannot parse the named imports in the production `world
# plugin`, but its types/lifecycle interfaces remain the canonical contract —
# strip the world, keep the interfaces (mirrors build-components.sh).
awk '
  /^world plugin[[:space:]]*\{/ { skipping=1; depth=1; next }
  skipping {
    opens=gsub(/\{/, "{"); closes=gsub(/\}/, "}"); depth += opens - closes
    if (depth == 0) skipping=0
    next
  }
  { print }
' "$SDK_WIT/plugin.wit" > "$deps/plugin.wit"
cp "$SDK_WIT"/deps/*.wit "$deps/"

cargo build --manifest-path "$PLUGIN/Cargo.toml" --target "$TARGET" --release
artifact="$PLUGIN/target/$TARGET/release/ryuzi_plugin_github.wasm"
test -f "$artifact"
printf '%s\n' "$artifact"
