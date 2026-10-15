import { useEffect, useState } from "react";
import { api, AppDetection, errText } from "../api";

const NOT_YET = [
  { name: "Codex Desktop / Claude Desktop", why: "Each desktop integration is verified individually after the CLI paths work." },
];

const INTEGRATION: Record<string, JSX.Element> = {
  codex_cli: (
    <>
      Integration: a PrismFlow-owned profile file (<code>~/.codex/prismflow-&lt;provider&gt;.config.toml</code>), used with <code>codex -p</code>. Your own <code>config.toml</code> is never edited.
      Providers without the Responses API (e.g. NVIDIA NIM) are reached through PrismFlow's local gateway.
    </>
  ),
  claude_code: (
    <>
      Integration: a PrismFlow-owned settings file (<code>~/.claude/prismflow-&lt;provider&gt;.settings.json</code>), used with <code>claude --settings</code>. Your own <code>settings.json</code> and login are never edited.
      Claude Code speaks only the Anthropic Messages API, so every provider goes through PrismFlow's local gateway; Claude Code holds only a revocable local token.
    </>
  ),
  aider: <>Launched by PrismFlow with <code>OPENAI_API_BASE</code> pointed at the local gateway and a token fetched at launch. Your own Aider settings are untouched.</>,
  cline: <>Launched with an isolated <code>--data-dir</code> inside PrismFlow's folder; the key comes from the environment at launch, so none is stored. Your <code>~/.cline</code> is untouched.</>,
  pi: <>Launched with an isolated <code>PI_CODING_AGENT_DIR</code>; Pi runs PrismFlow's token helper itself on each request. Your <code>~/.pi</code> is untouched.</>,
  hermes: <>Launched with an isolated <code>HERMES_HOME</code>; the token is passed by environment variable. Your <code>~/.hermes</code> is untouched. The first launch installs Hermes' Python dependencies into the isolated home.</>,
};

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
            <div className="small mt">{INTEGRATION[a.app_id]}</div>
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
