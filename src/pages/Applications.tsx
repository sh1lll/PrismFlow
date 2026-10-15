import { useEffect, useState } from "react";
import { api, AppDetection, errText } from "../api";

const NOT_YET = [
  { name: "Claude Code CLI", why: "Needs the compatibility gateway (Anthropic Messages ↔ OpenAI translation). Planned next." },
  { name: "Codex Desktop / Claude Desktop", why: "Each desktop integration is verified individually after the CLI paths work." },
];

export default function Applications() {
  const [apps, setApps] = useState<AppDetection[] | null>(null);
  const [error, setError] = useState("");
  const load = () => {
    setApps(null);
    api.applications().then(setApps).catch((e) => setError(errText(e)));
  };
  useEffect(load, []);

  return (
    <div>
      <div className="row spread">
        <h1>Applications</h1>
        <button className="ghost" onClick={load}>Re-detect</button>
      </div>
      {error && <div className="card err">{error}</div>}
      {!apps ? (
        <div className="card muted">Detecting…</div>
      ) : (
        apps.map((a) => (
          <div key={a.app_id} className="card">
            <div className="row spread">
              <b>{a.display_name}</b>
              {a.installed ? (
                <span className={`badge ${a.version_verified ? "ok" : "warn"}`}>
                  {a.version_verified ? "Supported version" : "Unverified version"}
                </span>
              ) : (
                <span className="badge err">Not installed</span>
              )}
            </div>
            {a.version && <div className="small">{a.version}</div>}
            {a.path && <div className="muted small"><code>{a.path}</code></div>}
            <div className="small mt">
              Integration: a PrismFlow-owned profile file (<code>~/.codex/prismflow-&lt;provider&gt;.config.toml</code>), used with <code>codex -p</code>. Your own <code>config.toml</code> is never edited.
            </div>
            {a.notes.map((n, i) => <div key={i} className="warn small">{n}</div>)}
          </div>
        ))
      )}
      <h2>Not supported in this build</h2>
      {NOT_YET.map((n) => (
        <div key={n.name} className="card muted">
          <b>{n.name}</b>
          <div className="small">{n.why}</div>
        </div>
      ))}
    </div>
  );
}
