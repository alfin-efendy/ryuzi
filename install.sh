#!/usr/bin/env sh
set -eu

REPO="alfin-efendy/herness-router"
INSTALL_DIR="${HR_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${HR_VERSION:-latest}"

err() { echo "install: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "required tool not found: $1"; }

need curl
need tar

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux)  goos="linux" ;;
  Darwin) goos="darwin" ;;
  *) err "unsupported OS: $os (use the Windows zip from GitHub Releases or Scoop)" ;;
esac

case "$arch" in
  x86_64|amd64) goarch="amd64" ;;
  aarch64|arm64) goarch="arm64" ;;
  *) err "unsupported arch: $arch" ;;
esac

# musl vs glibc (Linux only)
suffix=""
if [ "$goos" = "linux" ] && ldd --version 2>&1 | grep -qi musl; then
  suffix="_musl"
fi

if [ "$VERSION" = "latest" ]; then
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' | cut -d'"' -f4)
  [ -n "$tag" ] || err "could not resolve latest release tag"
else
  tag="$VERSION"
fi

ver="${tag#v}"
asset="harness-router_${ver}_${goos}_${goarch}${suffix}.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "install: downloading $asset ($tag)"
curl -fsSL "$base/$asset" -o "$tmp/$asset" || err "download failed: $base/$asset"
curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt" || err "checksums download failed"

# sha256sum compatibility: macOS ships shasum, not sha256sum
if ! command -v sha256sum >/dev/null 2>&1; then
  sha256sum() { shasum -a 256 "$@"; }
fi

# verify checksum
( cd "$tmp" && grep " $asset\$" checksums.txt | sha256sum -c - ) \
  || err "checksum verification failed for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 0755 "$tmp/hr" "$INSTALL_DIR/hr"

echo "install: hr installed to $INSTALL_DIR/hr"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "install: add $INSTALL_DIR to your PATH" ;;
esac
"$INSTALL_DIR/hr" --version
