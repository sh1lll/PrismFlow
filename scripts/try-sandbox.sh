#!/bin/sh
# Try PrismFlow without touching your real setup.
#
# Every file PrismFlow would change (its own data, Codex, Claude Code, Claude
# Desktop, VS Code, JetBrains, editors, MCP/rules) goes into ~/PrismFlow-sandbox
# instead. Delete that folder to undo everything.
#
# Not sandboxed: keys you add are saved in your Mac's Keychain (under
# "PrismFlow"), and Local models can add/delete models in your real Ollama.
#
#   sh scripts/try-sandbox.sh          # start (builds the app the first time)
#   sh scripts/try-sandbox.sh reset    # wipe the sandbox folder
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BOX="$HOME/PrismFlow-sandbox"
APP="$ROOT/src-tauri/target/debug/bundle/macos/PrismFlow.app"

if [ "$1" = reset ]; then
  pkill -f "PrismFlow.app/Contents/MacOS/prismflow" 2>/dev/null || true
  rm -rf "$BOX"
  echo "Sandbox wiped."
  exit 0
fi

if [ ! -x "$APP/Contents/MacOS/prismflow" ]; then
  echo "Building PrismFlow (first time only, a few minutes)…"
  (cd "$ROOT" && CARGO_INCREMENTAL=0 npx tauri build --debug --bundles app)
fi

mkdir -p "$BOX"/state "$BOX"/codex "$BOX"/claude "$BOX"/home "$BOX"/vscode "$BOX"/claude-desktop
pkill -f "PrismFlow.app/Contents/MacOS/prismflow" 2>/dev/null || true

PRISMFLOW_STATE_DIR="$BOX/state" \
CODEX_HOME="$BOX/codex" \
CLAUDE_CONFIG_DIR="$BOX/claude" \
PRISMFLOW_MCP_HOME="$BOX/home" \
PRISMFLOW_EDITORS_HOME="$BOX/home" \
PRISMFLOW_VSCODE_USER_DIR="$BOX/vscode" \
PRISMFLOW_JETBRAINS_ACP_FILE="$BOX/home/.jetbrains/acp.json" \
PRISMFLOW_CLAUDE_3P_DIR="$BOX/claude-desktop" \
PRISMFLOW_BROKER_PORT=47891 \
nohup "$APP/Contents/MacOS/prismflow" > "$BOX/prismflow.log" 2>&1 &

echo "PrismFlow (sandbox) is starting. Everything it changes is in $BOX"
echo "Quit it from the prism icon in the menu bar."
