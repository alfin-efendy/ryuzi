#!/usr/bin/env sh
set -eu

# Build ONE first-party LLM provider component (plugins/<id>) to wasm32-wasip2,
# materializing its wit/deps the same way the fixture builder
# (build-components.sh) and the release signer (scripts/plugins/build-first-party.ts)
# do.
#
# Usage: sh build-provider-component.sh <plugin-dir-name>
#   e.g. sh build-provider-component.sh openai
#
# Each `plugins/<id>` is a STANDALONE workspace crate (not a tests/fixtures
# fixture), so this is a sibling of build-components.sh rather than another
# `build_fixture` line. It touches only `plugins/<id>/wit/deps` (gitignored), so
# it never races build-components.sh's `wit/deps` rewrites of the fixtures. It
# is parameterized because the provider migration ships one bundle per
# OpenAI-format provider, all built identically.

if [ $# -ne 1 ]; then
  echo "usage: $0 <plugin-dir-name>" >&2
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
PLUGIN="$ROOT/plugins/$1"
SDK_WIT="$ROOT/crates/plugin-sdk/wit"
TARGET=wasm32-wasip2

if [ ! -f "$PLUGIN/Cargo.toml" ]; then
  echo "no such provider component: plugins/$1" >&2
  exit 1
fi

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
# cargo's artifact name is the crate name with `-` replaced by `_`.
stem=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$PLUGIN/Cargo.toml" | head -n 1 | tr '-' '_')
artifact="$PLUGIN/target/$TARGET/release/$stem.wasm"
test -f "$artifact"
printf '%s\n' "$artifact"
