import { useEffect, useState } from "react";
import { api, RuntimeSettings, UninstallReport, errText } from "../api";

type NumericKey = "rate_limit_per_minute" | "max_concurrency_per_provider" | "upstream_retries";
const FIELDS: { key: NumericKey; label: string; help: string; max: number }[] = [
  { key: "rate_limit_per_minute", label: "Requests per minute, per connection", help: "0 = no limit. Over the limit, the agent gets HTTP 429 from PrismFlow.", max: 10000 },
  { key: "max_concurrency_per_provider", label: "Concurrent requests, per provider", help: "0 = no limit. Extra requests wait up to 30 s.", max: 256 },
  { key: "upstream_retries", label: "Transport retries per attempt", help: "Retries for connection-level failures before a fallback is tried.", max: 5 },
];

export default function Settings() {
  const [s, setS] = useState<RuntimeSettings | null>(null);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  useEffect(() => { api.getSettings().then(setS).catch((e) => setMsg({ tone: "err", text: errText(e) })); }, []);

  async function save() {
    if (!s) return;
    try {
      setS(await api.setSettings(s));
      setMsg({ tone: "ok", text: "Saved. Engine-side limits restart the gateway briefly if it is running." });
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  return (
    <div>
      <h1>Settings</h1>
      <div className="card">
        <div className="section-title">Gateway limits</div>
        {!s ? <div className="muted">Loading…</div> : FIELDS.map((f) => (
          <div key={f.key} className="mt">
            <label className="small">{f.label}</label>
            <input type="number" min={0} max={f.max} value={s[f.key]}
              onChange={(e) => setS({ ...s, [f.key]: Math.max(0, Math.min(f.max, Number(e.target.value) || 0)) })} />
            <div className="small muted">{f.help}</div>
          </div>
        ))}
        <div className="row mt"><button className="primary" disabled={!s} onClick={save}>Save</button></div>
        {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
      </div>
      {s && <WebTools s={s} setS={setS} save={save} />}
      <Updates />
      <RemoveAll />
      <div className="card muted small">
        The gateway listens only on 127.0.0.1. Its port is fixed so agent configs stay valid; override it with the PRISMFLOW_BROKER_PORT environment variable.
      </div>
    </div>
  );
}

function RemoveAll() {
  const [plan, setPlan] = useState<UninstallReport | null>(null);
  const [result, setResult] = useState<UninstallReport | null>(null);
  const [typed, setTyped] = useState("");
  const [exported, setExported] = useState("");
  const [error, setError] = useState("");

  const items = (result ?? plan)?.items ?? [];
  return (
    <div className="card danger-zone">
      <div className="section-title">Remove PrismFlow from this computer</div>
      <div className="small muted">
        Disconnects every app (restoring its own settings), deletes PrismFlow's keys and tokens from the secure store,
        stops the gateway and deletes PrismFlow's data folder. Then drag PrismFlow to the Trash.
      </div>
      <div className="row mt">
        <button className="ghost" onClick={() => api.exportDiagnostics().then(setExported).catch((e) => setError(errText(e)))}>Export diagnostics first</button>
        <button className="ghost" onClick={() => api.uninstall(true).then((p) => { setPlan(p); setResult(null); }).catch((e) => setError(errText(e)))}>Show what will be removed</button>
      </div>
      {exported && <div className="small ok mt selectable">Saved to {exported}. Copy it somewhere else; the data folder is deleted next.</div>}
      {items.length > 0 && (
        <ul className="small mt">
          {items.map((i, n) => (
            <li key={n}><span className={i.outcome === "failed" ? "err" : i.outcome === "done" ? "ok" : "muted"}>{i.outcome}</span> {i.what}{i.detail && <span className="muted"> — {i.detail}</span>}</li>
          ))}
        </ul>
      )}
      {plan && !result && (
        <div className="row mt">
          <input placeholder='Type "remove" to confirm' value={typed} onChange={(e) => setTyped(e.target.value)} />
          <button className="danger" disabled={typed !== "remove"} onClick={() => api.uninstall(false).then(setResult).catch((e) => setError(errText(e)))}>Remove everything</button>
        </div>
      )}
      {result && (
        <div className={`small mt ${result.complete ? "ok" : "err"}`}>
          {result.complete ? "Done. Quit PrismFlow and move it to the Trash." : "Stopped at the failed step above; nothing after it was removed. Fix it and run again."}
        </div>
      )}
      {error && <div className="err small mt">{error}</div>}
    </div>
  );
}

function Updates() {
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [available, setAvailable] = useState<string | null>(null);
  async function check() {
    setMsg(null);
    try {
      const u = await api.checkUpdate();
      setAvailable(u.available);
      setMsg(u.available ? { tone: "ok", text: `PrismFlow ${u.available} is available (you have ${u.current}).${u.notes ? " " + u.notes : ""}` } : { tone: "ok", text: `PrismFlow ${u.current} is up to date.` });
    } catch (e) { setMsg({ tone: "muted", text: errText(e) }); }
  }
  return (
    <div className="card">
      <div className="section-title">Updates</div>
      <div className="small muted">Updates are downloaded only after you confirm, and installed only if their signature matches the key built into PrismFlow.</div>
      <div className="row mt">
        <button className="ghost" onClick={check}>Check for updates</button>
        {available && <button className="primary" onClick={() => api.installUpdate().catch((e) => setMsg({ tone: "err", text: errText(e) }))}>Install {available} and restart</button>}
      </div>
      {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

function WebTools({ s, setS, save }: { s: RuntimeSettings; setS: (s: RuntimeSettings) => void; save: () => void }) {
  const w = s.web_tools;
  const set = (patch: Partial<RuntimeSettings["web_tools"]>) => setS({ ...s, web_tools: { ...w, ...patch } });
  const [hasKey, setHasKey] = useState(false);
  const [key, setKey] = useState("");
  const [msg, setMsg] = useState("");
  useEffect(() => { api.hasWebSearchKey().then(setHasKey).catch(() => setHasKey(false)); }, []);
  return (
    <div className="card">
      <div className="section-title">Web tools for Claude Code</div>
      <div className="small muted">
        Claude Code's WebSearch normally runs on Anthropic's servers, so it fails with other providers. When this is on, PrismFlow runs
        searches and page fetches on this computer for gateway connections. Off by default.
      </div>
      <label className="small mt-block"><input type="checkbox" className="check" checked={w.enabled} onChange={(e) => set({ enabled: e.target.checked })} /> Run web search and fetch locally</label>
      <label className="small muted mt-block">Search backend</label>
      <select value={w.backend} onChange={(e) => set({ backend: e.target.value as RuntimeSettings["web_tools"]["backend"] })}>
        <option value="none">None (fetch only)</option>
        <option value="searxng">SearXNG (your own instance)</option>
        <option value="brave">Brave Search API</option>
      </select>
      {w.backend === "searxng" && <input className="mt" placeholder="https://search.example.org" value={w.searxng_url} onChange={(e) => set({ searxng_url: e.target.value })} />}
      {w.backend === "brave" && (
        <div className="row mt">
          <input type="password" placeholder={hasKey ? "Key stored — paste to replace" : "Brave Search API key"} value={key} onChange={(e) => setKey(e.target.value)} />
          <button className="ghost" onClick={() => api.setWebSearchKey(key).then(() => { setKey(""); setHasKey(true); setMsg("Key saved to the secure store."); }).catch((e) => setMsg(errText(e)))}>Save key</button>
        </div>
      )}
      <label className="small mt-block"><input type="checkbox" className="check" checked={w.allow_private} onChange={(e) => set({ allow_private: e.target.checked })} /> Allow fetching addresses on private networks</label>
      {w.allow_private && <div className="warn small">A model could then read services on your local network (routers, internal sites). Leave this off unless you need it.</div>}
      <div className="row mt"><button className="primary" onClick={save}>Save web tools</button></div>
      {msg && <div className="small mt">{msg}</div>}
    </div>
  );
}
