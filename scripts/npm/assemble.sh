#!/usr/bin/env bash
set -euo pipefail
# Extracts released tarballs (./out/ryuzi-<ver>-<triple>.tar.gz, from
# `gh release download`) into the npm platform package dirs.

declare -A MAP=(
  [x86_64-unknown-linux-gnu]=ryuzi-linux-x64
  [aarch64-unknown-linux-gnu]=ryuzi-linux-arm64
  [x86_64-unknown-linux-musl]=ryuzi-linux-x64-musl
  [aarch64-unknown-linux-musl]=ryuzi-linux-arm64-musl
  [x86_64-apple-darwin]=ryuzi-darwin-x64
  [aarch64-apple-darwin]=ryuzi-darwin-arm64
)
for triple in "${!MAP[@]}"; do
  tarball=$(ls out/ryuzi-*-"$triple".tar.gz)
  tmp=$(mktemp -d)
  tar -xzf "$tarball" -C "$tmp"
  install -m 0755 "$tmp/ryuzi" "npm/platform/${MAP[$triple]}/ryuzi"
  rm -rf "$tmp"
done
echo "OK: assembled npm packages"
