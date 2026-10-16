#!/bin/sh
# Install the pinned ai-gateway runtime into src-tauri/gateway/node_modules
# and verify it. Safe to re-run.
set -eu
cd "$(dirname "$0")/../src-tauri/gateway"

PIN_VERSION="1.0.21"
PIN_INTEGRITY="sha512-iZkW2V1qhEF/cMX8MXnGjOIsSSScsE9WaEfEVZZxsn7lLNykTBJ3VZnN6+iDFBU+P0nK+NO6hSS7I83rBSMnJw=="

# 1. The lockfile must still carry the reviewed integrity hash. npm ci then
#    refuses any tarball that does not match it.
grep -q "\"integrity\": \"$PIN_INTEGRITY\"" package-lock.json || {
  echo "fetch-gateway: package-lock.json no longer pins the reviewed ai-gateway tarball" >&2; exit 1; }

# 2. Install exactly the lockfile. Install scripts are disabled.
npm ci --omit=dev --ignore-scripts --no-audit --no-fund

# 3. Check the installed package's version and license text.
PKG=node_modules/@the-next-ai/ai-gateway
node -e '
const p=require("./'"$PKG"'/package.json");
if (p.version !== "'"$PIN_VERSION"'") { console.error("unexpected version " + p.version); process.exit(1); }
if (p.license !== "MIT") { console.error("unexpected license " + p.license); process.exit(1); }'
grep -q "^MIT License" "$PKG/LICENSE" || { echo "fetch-gateway: LICENSE is not MIT" >&2; exit 1; }
echo "ai-gateway $PIN_VERSION installed and verified (MIT, integrity pinned)."
