#!/usr/bin/env bash
set -euo pipefail
# Copies built binaries (./out) into the npm platform package dirs.
# Assumes scripts/build-binaries.sh already ran.

cp out/gnu/linux_amd64/hr        npm/platform/harness-router-linux-x64/hr
cp out/gnu/linux_arm64/hr        npm/platform/harness-router-linux-arm64/hr
cp out/musl/linux_amd64/hr       npm/platform/harness-router-linux-x64-musl/hr
cp out/musl/linux_arm64/hr       npm/platform/harness-router-linux-arm64-musl/hr
cp out/other/darwin_amd64/hr     npm/platform/harness-router-darwin-x64/hr
cp out/other/darwin_arm64/hr     npm/platform/harness-router-darwin-arm64/hr
cp out/other/windows_amd64/hr.exe npm/platform/harness-router-win32-x64/hr.exe

chmod +x npm/platform/harness-router-linux-x64/hr \
         npm/platform/harness-router-linux-arm64/hr \
         npm/platform/harness-router-linux-x64-musl/hr \
         npm/platform/harness-router-linux-arm64-musl/hr \
         npm/platform/harness-router-darwin-x64/hr \
         npm/platform/harness-router-darwin-arm64/hr
echo "OK: assembled npm packages"
