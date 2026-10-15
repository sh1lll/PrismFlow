#!/bin/sh
# Download the pinned Node.js runtime that release bundles ship for the
# gateway engine, verify it against nodejs.org's SHASUMS256.txt, and keep only
# the node binary and its LICENSE in src-tauri/runtime/node/.
#
#   scripts/fetch-node.sh [platform]   platform: darwin-arm64 | darwin-x64 |
#                                      linux-x64 | linux-arm64 | win-x64
# Default: the current machine. Requires curl, shasum/sha256sum, tar/unzip.
set -eu
NODE_VERSION="22.23.1"   # LTS line; bump deliberately and re-run the perf + real-agent smokes
BASE="https://nodejs.org/dist/v$NODE_VERSION"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/src-tauri/runtime/node"

plat="${1:-}"
if [ -z "$plat" ]; then
  os=$(uname -s); arch=$(uname -m)
  case "$os-$arch" in
    Darwin-arm64) plat=darwin-arm64 ;; Darwin-x86_64) plat=darwin-x64 ;;
    Linux-x86_64) plat=linux-x64 ;; Linux-aarch64|Linux-arm64) plat=linux-arm64 ;;
    *) echo "fetch-node: pass a platform explicitly" >&2; exit 2 ;;
  esac
fi
case "$plat" in
  win-x64) file="node-v$NODE_VERSION-$plat.zip" ;;
  darwin-*|linux-*) file="node-v$NODE_VERSION-$plat.tar.gz" ;;
  *) echo "fetch-node: unknown platform $plat" >&2; exit 2 ;;
esac

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$BASE/SHASUMS256.txt" -o "$tmp/SHASUMS256.txt"
curl -fsSL "$BASE/$file" -o "$tmp/$file"
want=$(grep " $file\$" "$tmp/SHASUMS256.txt" | cut -d' ' -f1)
[ -n "$want" ] || { echo "fetch-node: $file not listed in SHASUMS256.txt" >&2; exit 1; }
if command -v sha256sum >/dev/null; then got=$(sha256sum "$tmp/$file" | cut -d' ' -f1); else got=$(shasum -a 256 "$tmp/$file" | cut -d' ' -f1); fi
[ "$want" = "$got" ] || { echo "fetch-node: checksum mismatch for $file" >&2; exit 1; }

rm -rf "$OUT"; mkdir -p "$OUT"
dir="node-v$NODE_VERSION-$plat"
if [ "$plat" = win-x64 ]; then
  (cd "$tmp" && unzip -q "$file" "$dir/node.exe" "$dir/LICENSE")
  cp "$tmp/$dir/node.exe" "$OUT/node.exe"
else
  tar -xzf "$tmp/$file" -C "$tmp" "$dir/bin/node" "$dir/LICENSE"
  mkdir -p "$OUT/bin"; cp "$tmp/$dir/bin/node" "$OUT/bin/node"; chmod 755 "$OUT/bin/node"
fi
cp "$tmp/$dir/LICENSE" "$OUT/LICENSE"
echo "Node.js $NODE_VERSION ($plat) installed in src-tauri/runtime/node (sha256 $got)."
