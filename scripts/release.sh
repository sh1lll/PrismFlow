#!/bin/sh
# Build a signed, updatable PrismFlow release for the current platform.
# Refuses to run without the owner's signing material: no unsigned releases.
# See docs/RELEASE.md for how to create each secret.
#
# Required:
#   TAURI_SIGNING_PRIVATE_KEY (+ TAURI_SIGNING_PRIVATE_KEY_PASSWORD)  update signing
#   PRISMFLOW_UPDATER_PUBKEY        matching public key (embedded in the app)
#   PRISMFLOW_UPDATE_ENDPOINT       https URL of latest.json for this channel
# macOS also: APPLE_SIGNING_IDENTITY, APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
# Windows also: a code-signing certificate configured per docs/RELEASE.md
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
need() { eval "v=\${$1:-}"; [ -n "$v" ] || { echo "release: $1 is not set (docs/RELEASE.md)" >&2; exit 1; }; }
for v in TAURI_SIGNING_PRIVATE_KEY PRISMFLOW_UPDATER_PUBKEY PRISMFLOW_UPDATE_ENDPOINT; do need "$v"; done
case "$PRISMFLOW_UPDATE_ENDPOINT" in https://*) ;; *) echo "release: update endpoint must be https" >&2; exit 1 ;; esac
if [ "$(uname -s)" = Darwin ]; then
  for v in APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do need "$v"; done
fi

"$ROOT/scripts/fetch-gateway.sh"
"$ROOT/scripts/fetch-node.sh" "${PRISMFLOW_NODE_PLATFORM:-}"

conf="$(mktemp)"; trap 'rm -f "$conf"' EXIT
node -e '
const base = require(process.argv[1]);
base.bundle.createUpdaterArtifacts = true;
if (process.platform === "darwin") base.bundle.macOS.signingIdentity = process.env.APPLE_SIGNING_IDENTITY;
base.plugins = { updater: { pubkey: process.env.PRISMFLOW_UPDATER_PUBKEY, endpoints: [process.env.PRISMFLOW_UPDATE_ENDPOINT] } };
require("fs").writeFileSync(process.argv[2], JSON.stringify(base, null, 2));
' "$ROOT/src-tauri/tauri.bundle.conf.json" "$conf"

cd "$ROOT"
PRISMFLOW_RELEASE=1 npx tauri build --features updater --config "$conf"
echo "release: artifacts in src-tauri/target/release/bundle/ (upload them and latest.json to your update host)"
