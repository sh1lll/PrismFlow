import { useEffect, useState } from "react";
import { api, AppDetection, errText } from "../api";

/** One plain sentence per app: what connecting it does. Terminal agents
 *  use the first note from detection, which already says which mode it is. */
const WHAT: Record<string, string> = {
  codex_cli: "Uses your usual Codex chats. PrismFlow adds its own Codex profile; your Codex settings stay as they are.",
  codex_desktop: "Uses your usual Codex app, chats and all. PrismFlow switches its model and puts your old setting back when you disconnect.",
  codex_desktop_separate: "Opens the Codex app with its own chats and settings, just for PrismFlow. Your usual Codex stays as it is.",
  claude_code: "Uses your usual Claude Code chats. PrismFlow adds its own settings file; your login and settings stay as they are.",
  claude_code_separate: "Claude Code with its own chats, settings and MCP servers, just for PrismFlow.",
  codex_cli_separate: "Codex CLI with its own chats and settings, just for PrismFlow.",
  vscode_claude: "The Claude Code extension with its own chats, in your usual VS Code.",
  vscode_claude_shared: "The Claude Code extension with your usual Claude Code chats and settings.",
  vscode_separate: "A VS Code window of its own, with Claude Code and Chat set up for PrismFlow. Your extensions are shared.",
  zed_separate: "A Zed of its own with PrismFlow ready. Your usual Zed stays as it is.",
  claude_desktop: "PrismFlow switches your Claude Desktop to PrismFlow, and back to your Claude account when you disconnect.",
  claude_desktop_separate: "A second Claude Desktop just for PrismFlow. Your usual one stays signed in.",
};

export default function Applications() {
  const [apps, setApps] = useState<AppDetection[] | null>(null);
  const [error, setError] = useState("");
  const load = () => {
    setApps(null);
    api.applications().then(setApps).catch((e) => setError(errText(e)));
  };
  useEffect(load, []);

  const sorted = (apps ?? []).slice().sort((a, b) => Number(b.installed) - Number(a.installed));
  return (
    <div>
      <div className="row spread">
        <h1>Apps</h1>
        <button className="ghost" onClick={load}>Look again</button>
      </div>
      {error && <div className="card err">{error}</div>}
      {!apps ? <div className="card muted">Looking for apps…</div> : sorted.map((a) => (
        <div key={a.app_id} className="card compact">
          <div className="row spread">
            <b>{a.display_name}</b>
            <span className={`badge ${a.installed ? "ok" : "muted"}`}>{a.installed ? "Found" : "Not installed"}</span>
          </div>
          {a.installed && <div className="small muted">{WHAT[a.app_id] ?? a.notes[0]}</div>}
          {a.installed && !a.version_verified && <div className="small warn">This version is newer than the one PrismFlow was tested with. Press Test after connecting.</div>}
          {a.installed && (
            <details className="mt-s">
              <summary className="small">Details</summary>
              <div className="small muted selectable">{a.version} · {a.path}</div>
              {a.notes.slice(WHAT[a.app_id] ? 0 : 1).map((n, i) => <div key={i} className="small muted">{n}</div>)}
            </details>
          )}
        </div>
      ))}
      <details className="card">
        <summary className="small">Apps that can't use PrismFlow</summary>
        <div className="small mt-s"><b>Cursor</b> <span className="muted">sends your requests through Cursor's own servers, which can't reach your computer.</span></div>
        <div className="small mt-s"><b>Warp</b> <span className="muted">refuses AI addresses on your own computer.</span></div>
        <div className="small mt-s"><b>Windsurf</b> <span className="muted">has no setting for your own AI address.</span></div>
        <div className="small mt-s"><b>Gemini CLI</b> <span className="muted">only speaks Google's own format, which PrismFlow doesn't offer yet.</span></div>
        <div className="small muted mt-s">You can still share MCP servers with Cursor and Windsurf on the Tools page.</div>
      </details>
    </div>
  );
}
