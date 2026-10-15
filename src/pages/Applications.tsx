import { useEffect, useState } from "react";
import { api, AppDetection, errText } from "../api";

/** One plain sentence per app: what connecting it does. */
const WHAT: Record<string, string> = {
  codex_cli: "PrismFlow adds its own Codex profile. Your Codex settings stay as they are.",
  codex_desktop: "PrismFlow switches the Codex app's model and puts your old setting back when you disconnect.",
  claude_code: "PrismFlow adds its own Claude Code settings file. Your login and settings stay as they are.",
  claude_desktop: "PrismFlow shows you what to enter in Claude Desktop. It doesn't change the app itself.",
  aider: "PrismFlow starts Aider with its own settings. Your Aider setup stays as it is.",
  cline: "PrismFlow starts Cline with its own settings. Your Cline setup stays as it is.",
  pi: "PrismFlow starts Pi with its own settings. Your Pi setup stays as it is.",
  hermes: "PrismFlow starts Hermes with its own settings. Your Hermes setup stays as it is.",
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
          {a.installed && <div className="small muted">{WHAT[a.app_id] ?? ""}</div>}
          {a.installed && !a.version_verified && <div className="small warn">This version is newer than the one PrismFlow was tested with. Press Test after connecting.</div>}
          {a.installed && (
            <details className="mt-s">
              <summary className="small">Details</summary>
              <div className="small muted selectable">{a.version} · {a.path}</div>
              {a.notes.map((n, i) => <div key={i} className="small muted">{n}</div>)}
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
