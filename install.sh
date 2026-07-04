#!/usr/bin/env sh
set -eu

REPO="alfin-efendy/ryuzi"
INSTALL_DIR="${RYUZI_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${RYUZI_VERSION:-latest}"

err() { echo "install: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "required tool not found: $1"; }

need curl
need tar

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux)  ;;
  Darwin) ;;
  *) err "unsupported OS: $os" ;;
esac

case "$arch" in
  x86_64|amd64)  cpu="x86_64" ;;
  aarch64|arm64) cpu="aarch64" ;;
  *) err "unsupported architecture: $arch" ;;
esac

if [ "$os" = "Darwin" ]; then
  triple="${cpu}-apple-darwin"
else
  libc="gnu"
  if ldd --version 2>&1 | grep -qi musl; then
    libc="musl"
  fi
  triple="${cpu}-unknown-linux-${libc}"
fi

if [ "$VERSION" = "latest" ]; then
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' | cut -d'"' -f4)
  [ -n "$tag" ] || err "could not resolve latest release tag"
else
  tag="$VERSION"
fi

# reject anything that isn't a plain version tag (e.g. v1.2.3, 1.2.3, v1.2.3-rc.1)
case "$tag" in
  *[!A-Za-z0-9._-]*|"") err "invalid version/tag: '$tag'" ;;
esac

ver="${tag#v}"
asset="ryuzi-${ver}-${triple}.tar.gz"
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
install -m 0755 "$tmp/ryuzi" "$INSTALL_DIR/ryuzi"

echo "install: ryuzi installed to $INSTALL_DIR/ryuzi"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "install: add $INSTALL_DIR to your PATH" ;;
esac
"$INSTALL_DIR/ryuzi" --version
