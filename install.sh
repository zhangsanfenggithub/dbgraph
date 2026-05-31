#!/bin/sh
set -eu

REPO="${DBG_REPO:-https://github.com/zhangsanfenggithub/dbgraph}"
VERSION="${DBG_VERSION:-latest}"
INSTALL_DIR="${DBG_INSTALL_DIR:-$HOME/.local/bin}"

usage() {
  cat <<'EOF'
DbGraph installer

Usage:
  install.sh [--version VERSION] [--install-dir DIR]

Environment:
  DBG_REPO         GitHub repository URL
  DBG_VERSION      Release version or latest
  DBG_INSTALL_DIR  Install directory
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || { echo "dbgraph: --version requires a value" >&2; exit 2; }
      VERSION="$2"
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || { echo "dbgraph: --install-dir requires a value" >&2; exit 2; }
      INSTALL_DIR="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "dbgraph: unknown option $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Darwin/x86_64|Darwin/amd64) target="x86_64-apple-darwin" ;;
  Darwin/arm64|Darwin/aarch64) target="aarch64-apple-darwin" ;;
  Linux/x86_64|Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux/arm64|Linux/aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) echo "dbgraph: unsupported platform $os/$arch" >&2; exit 1 ;;
esac
tag="$VERSION"
if [ "$tag" = "latest" ]; then
  tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$REPO/releases/latest" | sed -n 's#.*/releases/tag/##p')"
fi
[ -n "$tag" ] || { echo "dbgraph: could not resolve release version" >&2; exit 1; }
case "$tag" in v*) ;; *) tag="v$tag" ;; esac

asset="dbgraph-$tag-$target.tar.gz"
base="$REPO/releases/download/$tag"
url="$base/$asset"
checksum_url="$url.sha256"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

echo "Installing DbGraph $tag ($target)..."
curl -fsSL "$url" -o "$tmp/$asset"
curl -fsSL "$checksum_url" -o "$tmp/$asset.sha256"

expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || { echo "dbgraph: sha256 checksum mismatch" >&2; exit 1; }

mkdir -p "$tmp/extract" "$INSTALL_DIR"
tar -xzf "$tmp/$asset" -C "$tmp/extract"

binary="$tmp/extract/dbgraph"
if [ ! -f "$binary" ]; then
  binary="$(find "$tmp/extract" -type f -name dbgraph | head -n 1)"
fi
[ -n "$binary" ] && [ -f "$binary" ] || { echo "dbgraph: archive did not contain dbgraph binary" >&2; exit 1; }

chmod +x "$binary"
cp "$binary" "$INSTALL_DIR/dbgraph.tmp"
mv "$INSTALL_DIR/dbgraph.tmp" "$INSTALL_DIR/dbgraph"

echo "Installed $INSTALL_DIR/dbgraph"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo ""
    echo "$INSTALL_DIR is not on PATH. Add it with:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
echo "Run: dbgraph --help"
