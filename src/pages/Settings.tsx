import { useEffect, useState } from "react";
import { api, RuntimeSettings, SelfCheckItem, UninstallReport, errText } from "../api";

type NumericKey = "rate_limit_per_minute" | "max_concurrency_per_provider" | "upstream_retries";
const FIELDS: { key: NumericKey; label: string; help: string; max: number }[] = [
  { key: "rate_limit_per_minute", label: "Most requests per minute (per app)", help: "0 means no limit.", max: 10000 },
  { key: "max_concurrency_per_provider", label: "Most requests at once (per provider)", help: "0 means no limit. Extra requests wait their turn.", max: 256 },
  { key: "upstream_retries", label: "Retries when a connection drops", help: "How many times to try again before giving up.", max: 5 },
];

/** Checks that PrismFlow itself works on this computer. */
function SelfCheck() {
  const [checks, setChecks] = useState<SelfCheckItem[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function run() {
    setBusy(true); setError("");
    try { setChecks(await api.selfCheck()); } catch (e) { setError(errText(e)); } finally { setBusy(false); }
  }
  return (
    <div className="card">
      <div className="row spread">
        <div className="section-title">Check PrismFlow</div>
        <button className="ghost" disabled={busy} onClick={run}>{busy ? "Checking…" : "Run check"}</button>
      </div>
      {checks?.map((c) => <div key={c.name} className={`small ${c.ok ? "" : "err"}`}>{c.ok ? "✓" : "✗"} {c.name}: {c.detail}</div>)}
      {error && <div className="small err">{error}</div>}
    </div>
  );
}

export default function Settings() {
  const [s, setS] = useState<RuntimeSettings | null>(null);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  useEffect(() => { api.getSettings().then(setS).catch((e) => setMsg({ tone: "err", text: errText(e) })); }, []);

  async function save() {
    if (!s) return;
    try {
      setS(await api.setSettings(s));
      setMsg({ tone: "ok", text: "Saved." });
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  return (
    <div>
      <h1>Settings</h1>
      <SelfCheck />
      <StartUp />
      <BackupCard />
      <div className="card">
        <div className="section-title">Limits</div>
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
      {s && (
        <div className="card">
          <div className="section-title">Speed up Claude Code</div>
          <div className="small muted">Claude Code sends small extra requests, like naming your chat. PrismFlow answers these itself, so your model only works on what you asked.</div>
          <label className="small mt-block"><input type="checkbox" className="check" checked={s.shortcuts} onChange={(e) => setS({ ...s, shortcuts: e.target.checked })} /> On</label>
          <div className="row mt"><button className="primary" onClick={save}>Save</button></div>
        </div>
      )}
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
      <div className="section-title">Remove PrismFlow</div>
      <div className="small muted">Undoes everything PrismFlow set up and deletes its saved keys and data. Your own app settings stay.</div>
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
          {result.complete ? "Done. Quit PrismFlow and move it to the Trash." : "Stopped at the step marked failed. Nothing after it was removed."}
        </div>
      )}
      {error && <div className="err small mt">{error}</div>}
    </div>
  );
}

function BackupCard() {
  const [path, setPath] = useState("");
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <div className="card">
      <div className="section-title">Backup</div>
      <div className="small muted">Save your setup to a file, or bring it back on another computer. Keys aren't included; add or import them after restoring.</div>
      <div className="row mt"><button className="ghost" onClick={() => api.backupExport().then((p) => setMsg({ tone: "ok", text: `Saved to ${p}` })).catch((e) => setMsg({ tone: "err", text: errText(e) }))}>Save a backup</button></div>
      <div className="row mt">
        <input placeholder="Path to a PrismFlow backup file" value={path} onChange={(e) => setPath(e.target.value)} />
        <button className="ghost" disabled={!path.trim() || busy} onClick={() => {
          setBusy(true);
          api.backupRestore(path).then((r) => setMsg({ tone: r.problems.length ? "warn" : "ok", text: `Restored: ${r.restored.join(", ")}${r.problems.length ? `\n\nNeeds attention:\n${r.problems.join("\n")}` : ""}` }))
            .catch((e) => setMsg({ tone: "err", text: errText(e) })).finally(() => setBusy(false));
        }}>{busy ? "Restoring…" : "Restore"}</button>
      </div>
      {msg && <div className={`small mt pre-wrap selectable ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

function StartUp() {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => { api.autostartGet().then(setOn).catch(() => setOn(false)); }, []);
  return (
    <div className="card">
      <div className="section-title">Start-up</div>
      <div className="small muted">PrismFlow lives in the menu bar. Closing its window keeps your apps working; quit from the menu bar icon.</div>
      <label className="small mt-block">
        <input type="checkbox" className="check" disabled={on === null} checked={!!on}
          onChange={(e) => { const v = e.target.checked; api.autostartSet(v).then(() => setOn(v)).catch(() => {}); }} />
        Open PrismFlow when I log in (in the background)
      </label>
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
      <div className="small muted">Updates install only after you confirm, and only if they really come from PrismFlow.</div>
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
      <div className="section-title">Web search for Claude Code</div>
      <div className="small muted">Lets Claude Code search the web when it uses a non-Anthropic model. Off by default.</div>
      <label className="small mt-block"><input type="checkbox" className="check" checked={w.enabled} onChange={(e) => set({ enabled: e.target.checked })} /> Let apps search the web</label>
      <label className="small muted mt-block">Search backend</label>
      <select value={w.backend} onChange={(e) => set({ backend: e.target.value as RuntimeSettings["web_tools"]["backend"] })}>
        <option value="none">None (only open pages)</option>
        <option value="searxng">SearXNG (your own search server)</option>
        <option value="brave">Brave Search API</option>
      </select>
      {w.backend === "searxng" && <input className="mt" placeholder="Your SearXNG address" value={w.searxng_url} onChange={(e) => set({ searxng_url: e.target.value })} />}
      {w.backend === "brave" && (
        <div className="row mt">
          <input type="password" placeholder={hasKey ? "Key saved — paste to change" : "Brave Search key"} value={key} onChange={(e) => setKey(e.target.value)} />
          <button className="ghost" onClick={() => api.setWebSearchKey(key).then(() => { setKey(""); setHasKey(true); setMsg("Key saved."); }).catch((e) => setMsg(errText(e)))}>Save key</button>
        </div>
      )}
      <label className="small mt-block"><input type="checkbox" className="check" checked={w.allow_private} onChange={(e) => set({ allow_private: e.target.checked })} /> Also allow addresses on my home or work network</label>
      {w.allow_private && <div className="warn small">The AI could then read things like your router page. Leave this off unless you need it.</div>}
      <div className="row mt"><button className="primary" onClick={save}>Save web tools</button></div>
      {msg && <div className="small mt">{msg}</div>}
    </div>
  );
}
