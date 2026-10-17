import { useEffect, useState } from "react";
import { api, RuntimeSettings, SelfCheckItem, UninstallReport, errText } from "../api";

type NumericKey = "rate_limit_per_minute" | "max_concurrency_per_provider" | "upstream_retries";
const FIELDS: { key: NumericKey; label: string; max: number }[] = [
  { key: "rate_limit_per_minute", label: "Requests per minute, per app (0 = no limit)", max: 10000 },
  { key: "max_concurrency_per_provider", label: "Requests at once, per provider (0 = no limit)", max: 256 },
  { key: "upstream_retries", label: "Retries when a connection drops", max: 5 },
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
      <Updates />
      <BackupCard />
      <details className="card">
        <summary className="section-title">More settings</summary>
        {!s ? <div className="muted">Loading…</div> : (
          <>
            <label className="small mt-block"><input type="checkbox" className="check" checked={s.shortcuts} onChange={(e) => setS({ ...s, shortcuts: e.target.checked })} /> Answer Claude Code's small side requests myself (faster)</label>
            {FIELDS.map((f) => (
              <div key={f.key} className="mt">
                <label className="small">{f.label}</label>
                <input type="number" min={0} max={f.max} value={s[f.key]}
                  onChange={(e) => setS({ ...s, [f.key]: Math.max(0, Math.min(f.max, Number(e.target.value) || 0)) })} />
              </div>
            ))}
            <WebTools s={s} setS={setS} />
            <div className="row mt"><button className="primary" onClick={save}>Save</button></div>
            {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
          </>
        )}
      </details>
      <RemoveAll />
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
      <div className="small muted">Undoes PrismFlow's changes and deletes its keys. Your apps keep their own settings.</div>
      <div className="row mt">
        <button className="ghost" onClick={() => api.uninstall(true).then((p) => { setPlan(p); setResult(null); }).catch((e) => setError(errText(e)))}>Remove…</button>
        <button className="ghost" onClick={() => api.exportDiagnostics().then(setExported).catch((e) => setError(errText(e)))}>Save a problem report</button>
      </div>
      {exported && <div className="small ok mt selectable">Saved to {exported}</div>}
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
      <div className="small muted">Keys aren't included.</div>
      <div className="row mt"><button className="ghost" onClick={() => api.backupExport().then((p) => setMsg({ tone: "ok", text: `Saved to ${p}` })).catch((e) => setMsg({ tone: "err", text: errText(e) }))}>Save a backup</button></div>
      <details><summary className="small">Restore a backup</summary>
      <div className="row mt-s">
        <input placeholder="Path to a PrismFlow backup file" value={path} onChange={(e) => setPath(e.target.value)} />
        <button className="ghost" disabled={!path.trim() || busy} onClick={() => {
          setBusy(true);
          api.backupRestore(path).then((r) => setMsg({ tone: r.problems.length ? "warn" : "ok", text: `Restored: ${r.restored.join(", ")}${r.problems.length ? `\n\nNeeds attention:\n${r.problems.join("\n")}` : ""}` }))
            .catch((e) => setMsg({ tone: "err", text: errText(e) })).finally(() => setBusy(false));
        }}>{busy ? "Restoring…" : "Restore"}</button>
      </div>
      </details>
      {msg && <div className={`small mt pre-wrap selectable ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

function StartUp() {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => { api.autostartGet().then(setOn).catch(() => setOn(false)); }, []);
  return (
    <div className="card">
      <label className="small">
        <input type="checkbox" className="check" disabled={on === null} checked={!!on}
          onChange={(e) => { const v = e.target.checked; api.autostartSet(v).then(() => setOn(v)).catch(() => {}); }} />
        Start PrismFlow when I log in
      </label>
    </div>
  );
}

function Updates() {
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [available, setAvailable] = useState<string | null>(null);
  const [busy, setBusy] = useState<"" | "check" | "install">("");
  async function check() {
    setMsg(null); setBusy("check");
    try {
      const u = await api.checkUpdate();
      setAvailable(u.available);
      setMsg(u.available ? { tone: "ok", text: `Version ${u.available} is ready (you have ${u.current}).` } : { tone: "ok", text: `Up to date (${u.current}).` });
    } catch { setMsg({ tone: "muted", text: "Couldn't check for updates. Check your internet connection and try again." }); }
    finally { setBusy(""); }
  }
  return (
    <div className="card">
      <div className="row spread">
        <div className="section-title">Updates</div>
        <button className="ghost" disabled={busy !== ""} onClick={check}>{busy === "check" ? "Checking…" : "Check"}</button>
      </div>
      {available && <button className="primary mt-s" disabled={busy !== ""} onClick={() => { setBusy("install"); api.installUpdate().catch(() => setMsg({ tone: "err", text: "The update didn't download. Check your internet connection and try again." })).finally(() => setBusy("")); }}>{busy === "install" ? "Installing…" : `Install ${available} and restart`}</button>}
      {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

function WebTools({ s, setS }: { s: RuntimeSettings; setS: (s: RuntimeSettings) => void }) {
  const w = s.web_tools;
  const set = (patch: Partial<RuntimeSettings["web_tools"]>) => setS({ ...s, web_tools: { ...w, ...patch } });
  const [hasKey, setHasKey] = useState(false);
  const [key, setKey] = useState("");
  const [msg, setMsg] = useState("");
  useEffect(() => { api.hasWebSearchKey().then(setHasKey).catch(() => setHasKey(false)); }, []);
  return (
    <div className="mt">
      <label className="small mt-block"><input type="checkbox" className="check" checked={w.enabled} onChange={(e) => set({ enabled: e.target.checked })} /> Let Claude Code search the web on other models</label>
      {w.enabled && (
        <>
          <select className="mt-s" value={w.backend} onChange={(e) => set({ backend: e.target.value as RuntimeSettings["web_tools"]["backend"] })}>
            <option value="none">Open pages only</option>
            <option value="searxng">SearXNG (your own)</option>
            <option value="brave">Brave Search</option>
          </select>
          {w.backend === "searxng" && <input className="mt-s" placeholder="SearXNG address" value={w.searxng_url} onChange={(e) => set({ searxng_url: e.target.value })} />}
          {w.backend === "brave" && (
            <div className="row mt-s">
              <input type="password" placeholder={hasKey ? "Key saved" : "Brave Search key"} value={key} onChange={(e) => setKey(e.target.value)} />
              <button className="ghost" disabled={!key.trim()} onClick={() => api.setWebSearchKey(key).then(() => { setKey(""); setHasKey(true); setMsg("Key saved."); }).catch((e) => setMsg(errText(e)))}>Save key</button>
            </div>
          )}
          <label className="small mt-block"><input type="checkbox" className="check" checked={w.allow_private} onChange={(e) => set({ allow_private: e.target.checked })} /> Allow pages on my own network</label>
          {msg && <div className="small mt-s">{msg}</div>}
        </>
      )}
    </div>
  );
}
