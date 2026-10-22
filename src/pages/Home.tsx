import { useEffect, useState } from "react";
import {
  api, AppDetection, AppId, Connection, LaunchInfo, LimitState, Provider, appLabel, errText, isSeparate,
  modePair, providerName, shortStatus, statusTone,
} from "../api";
import { EditorGuide, GuidedSetup, ModelPicker, Stages, ask } from "./bits";
import RoutingPanel from "./RoutingPanel";
import TestRunning from "./TestRunning";

/** Your apps, each with what it uses now and one control to change it. */
export default function Home({ onSetup }: { onSetup: (app?: AppId) => void }) {
  const [conns, setConns] = useState<Connection[] | null>(null);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [apps, setApps] = useState<AppDetection[] | null>(null);
  const [error, setError] = useState("");
  const load = () => Promise.all([api.connections(), api.providers()])
    .then(([c, p]) => { setConns(c); setProviders(p); setError(""); })
    .catch((e) => setError(errText(e)));
  useEffect(() => { load(); api.applications().then(setApps).catch(() => setApps([])); }, []);
  // Rate limits and used-up quotas, checked every few seconds.
  const [limits, setLimits] = useState<LimitState[]>([]);
  useEffect(() => {
    const tick = () => api.providerLimits().then(setLimits).catch(() => {});
    tick();
    const t = setInterval(tick, 5000);
    return () => clearInterval(t);
  }, []);

  // Installed apps not set up yet; an app's separate twin isn't listed on its own.
  const used = new Set((conns ?? []).map((c) => appLabel(c.app_id)));
  const installed = new Set((apps ?? []).filter((a) => a.installed).map((a) => a.app_id));
  const others = (apps ?? []).filter((a) => a.installed && !isSeparate(a.app_id) && !used.has(appLabel(a.app_id)));

  return (
    <div>
      <div className="row spread head">
        <h1>Your apps</h1>
        <button className="primary" onClick={() => onSetup()}>Add app</button>
      </div>
      {error && <div className="card err small">{error}</div>}
      {!conns && !error && <div className="muted">Loading…</div>}
      {conns && conns.length === 0 && (
        <div className="card empty">
          <div className="empty-title">Connect your first app</div>
          <div className="muted">Pick an app, a provider and a model. It takes a minute.</div>
          <button className="primary mt" onClick={() => onSetup()}>Get started</button>
        </div>
      )}
      {conns && conns.length > 0 && (
        <div className="list">
          {conns.map((c) => <AppRow key={c.id} c={c} providers={providers} installed={installed} onChange={load}
            limit={limits.find((l) => l.provider_id === c.provider_id || l.provider_id === `conn:${c.id}`)} />)}
        </div>
      )}
      {others.length > 0 && (
        <>
          <h2>Also on this computer</h2>
          <div className="list">
            {others.map((a) => (
              <div key={a.app_id} className="app-row">
                <span className="dot muted" />
                <div className="grow"><div className="app-name">{appLabel(a.app_id)}</div></div>
                <button className="ghost" onClick={() => onSetup(a.app_id as AppId)}>Set up</button>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

/** "Rate limited · 37 s" / "Out of quota", counting down. */
function limitLabel(l: LimitState, now: number): string {
  const left = l.until ? Math.max(0, l.until - Math.floor(now / 1000)) : null;
  const wait = left === null ? "" : left < 90 ? ` · ${Math.max(1, left)} s` : left < 3600 ? ` · ${Math.round(left / 60)} min` : ` · ${Math.round(left / 3600)} h`;
  if (l.kind === "quota") return `Out of quota${wait}`;
  if (l.kind === "prismflow_limit") return `PrismFlow limit${wait}`;
  return `Rate limited${wait}`;
}

function limitHelp(l: LimitState, provider: string): string {
  const said = l.provider_message ? ` ${provider} said: "${l.provider_message}"` : "";
  if (l.kind === "quota") return `Your plan's or account's allowance at ${provider} is used up. It works again when it resets or you add credit.${said}`;
  if (l.kind === "prismflow_limit") return "This app sent more requests per minute than PrismFlow's Settings allow. It clears in a moment, or raise the limit in Settings → Gateway.";
  return `${provider} is getting too many requests right now. It clears by itself shortly; apps wait and retry.${said}`;
}

function AppRow({ c, providers, installed, onChange, limit }: { c: Connection; providers: Provider[]; installed: Set<string>; onChange: () => void; limit?: LimitState }) {
  const [panel, setPanel] = useState<"" | "change" | "more">("");
  const [busy, setBusy] = useState("");
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [launch, setLaunch] = useState<LaunchInfo | null>(null);
  useEffect(() => { api.launchInfo(c.id).then(setLaunch).catch(() => setLaunch(null)); }, [c.id]);
  const name = appLabel(c.app_id);
  // Ticks the limit countdown once a second while one is shown.
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!limit) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [limit]);

  async function run(label: string, f: () => Promise<unknown>, ok?: string) {
    setBusy(label); setMsg(null);
    try { await f(); if (ok) setMsg({ tone: "ok", text: ok }); onChange(); }
    catch (e) { setMsg({ tone: "err", text: errText(e) }); }
    finally { setBusy(""); }
  }

  return (
    <div className={`app-row-wrap ${panel ? "open" : ""}`}>
      <div className="app-row">
        <span className={`dot ${statusTone(c.status)}`} title={shortStatus(c.status)} />
        <div className="grow">
          <div className="app-name">{name}{isSeparate(c.app_id) && <span className="tag">Separate</span>}</div>
          <div className="small muted">{providerName(providers, c.provider_id)} · {c.model}</div>
        </div>
        {limit
          ? <span className={`small ${limit.kind === "quota" ? "err" : "warn"}`} title={limitHelp(limit, providerName(providers, c.provider_id))}>{limitLabel(limit, now)}</span>
          : <span className={`small ${statusTone(c.status)}`}>{shortStatus(c.status)}</span>}
        <button className="ghost" onClick={() => setPanel(panel === "change" ? "" : "change")}>Change</button>
        <button className="ghost icon" aria-label="More" onClick={() => setPanel(panel === "more" ? "" : "more")}>⋯</button>
      </div>

      {panel === "change" && (
        <ChangePanel c={c} providers={providers} onRefresh={onChange} onDone={(text) => { setPanel(""); setMsg({ tone: "ok", text }); onChange(); }} />
      )}

      {panel === "more" && (
        <div className="panel">
          {limit && <div className={`small ${limit.kind === "quota" ? "err" : "warn"}`}>{limitHelp(limit, providerName(providers, c.provider_id))}</div>}
          <div className="row wrap">
            <button className="ghost" disabled={busy !== ""} onClick={() => run("test", () => api.verify(c.id))}>{busy === "test" ? "Testing…" : "Test"}</button>
            {launch && launch.opens !== "Nothing" && (
              <button className="ghost" disabled={busy !== ""} onClick={() => run("open", () => api.launch(c.id), `Opened ${name}.`)}>Open</button>
            )}
            <SeparateToggle c={c} installed={installed} busy={busy !== ""} run={run} />
            <button className="ghost danger-text" disabled={busy !== ""}
              onClick={async () => { if (await ask(`Remove ${name} from PrismFlow?`, "Remove")) run("remove", () => api.disconnect(c.id)); }}>Remove</button>
          </div>
          {busy === "test" && <TestRunning id={c.id} />}
          {c.last_report && c.status !== "ConnectedAndVerified" && busy !== "test" && <Stages stages={c.last_report.stages} />}
          {c.route === "gateway" && <AppModels c={c} providers={providers} onSaved={onChange} />}
          {c.app_id === "claude_desktop" && <GuidedSetup id={c.id} />}
          {(c.app_id === "zed" || c.app_id === "continue_ide") && <EditorGuide id={c.id} />}
          {c.route === "gateway" && <details><summary className="small">Advanced</summary><RoutingPanel c={c} providers={providers} onSaved={onChange} />
            {launch?.line && <div className="small muted mt-s">Start it yourself: <code className="selectable">{launch.line}</code></div>}
          </details>}
        </div>
      )}
      {msg && <div className={`small row-msg ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

/** Which of the provider's models the app shows in its own model menu. */
function AppModels({ c, providers, onSaved }: { c: Connection; providers: Provider[]; onSaved: () => void }) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState<string[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set(c.app_models?.length ? c.app_models : [c.model]));
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const provider = providers.find((p) => p.id === c.provider_id);
  useEffect(() => {
    if (!open || all) return;
    api.discoverModels(c.provider_id)
      .then((m) => setAll(Array.from(new Set([c.model, ...(c.app_models ?? []), ...m.map((x) => x.id)]))))
      .catch(() => setAll(Array.from(new Set([c.model, ...(c.app_models ?? [])]))));
  }, [open]);
  const shown = (all ?? []).filter((m) => !filter || m.toLowerCase().includes(filter.toLowerCase())).slice(0, 300);
  const count = c.app_models?.length || 1;
  async function save() {
    setBusy(true); setMsg(null);
    try {
      await api.setAppModels(c.id, [c.model, ...Array.from(picked).filter((m) => m !== c.model)]);
      setMsg({ tone: "ok", text: `Saved. Restart ${appLabel(c.app_id)} to see the new list.` });
      onSaved();
    } catch (e) { setMsg({ tone: "err", text: errText(e) }); }
    finally { setBusy(false); }
  }
  return (
    <details onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}>
      <summary className="small">Models in the app ({count})</summary>
      <div className="small muted mt-s">Pick models in {appLabel(c.app_id)}'s own model menu instead of switching here. {c.model} stays the default.</div>
      {!all ? <div className="small muted mt-s">Loading {provider?.display_name ?? ""} models…</div> : (
        <>
          <div className="row mt-s">
            <input placeholder="Search models" value={filter} onChange={(e) => setFilter(e.target.value)} />
            <button className="ghost" onClick={() => setPicked(new Set([c.model, ...shown]))}>All</button>
            <button className="ghost" onClick={() => setPicked(new Set([c.model]))}>None</button>
          </div>
          <div className="model-list mt-s">
            {shown.map((m) => (
              <label key={m} className="small model">
                <input type="checkbox" className="check" disabled={m === c.model} checked={picked.has(m)}
                  onChange={(e) => { const n = new Set(picked); if (e.target.checked) n.add(m); else n.delete(m); setPicked(n); }} />
                {m}{m === c.model && <span className="muted"> · default</span>}
              </label>
            ))}
          </div>
          <div className="row mt-s">
            <button className="primary" disabled={busy} onClick={save}>{busy ? "Saving…" : `Show ${picked.size} in the app`}</button>
            {picked.size > 100 && <span className="small warn">Only the first 100 are shown.</span>}
          </div>
        </>
      )}
      {msg && <div className={`small mt-s ${msg.tone}`}>{msg.text}</div>}
    </details>
  );
}

/** Switch to another provider and/or model. */
function ChangePanel({ c, providers, onDone, onRefresh }: { c: Connection; providers: Provider[]; onDone: (msg: string) => void; onRefresh: () => void }) {
  const usable = providers.filter((p) => p.routes[c.app_id as AppId] != null && (p.has_credential || p.key_optional));
  const [pid, setPid] = useState(c.provider_id);
  const [model, setModel] = useState(c.model);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const provider = usable.find((p) => p.id === pid);
  async function apply() {
    setBusy(true); setError("");
    try {
      const n = await api.switchConnection(c.id, pid, model.trim());
      const running = await api.runningSessions(n.id).catch(() => []);
      onDone(running.length > 0 ? `Switched. Restart ${appLabel(c.app_id)} to use it.` : "Switched.");
    } catch (e) { setError(errText(e)); onRefresh(); }
    finally { setBusy(false); }
  }
  return (
    <div className="panel">
      <select value={pid} onChange={(e) => { setPid(e.target.value); setModel(""); }}>
        {usable.map((p) => <option key={p.id} value={p.id}>{p.display_name}</option>)}
      </select>
      {provider && <ModelPicker key={provider.id} provider={provider} value={model} onChange={setModel} />}
      <div className="row mt">
        <button className="primary" disabled={busy || !model.trim() || (pid === c.provider_id && model.trim() === c.model)} onClick={apply}>{busy ? "Switching…" : "Switch"}</button>
        {usable.length < providers.filter((p) => p.routes[c.app_id as AppId] != null).length && <span className="small muted">Add keys on Providers for more choices.</span>}
      </div>
      {error && <div className="err small mt-s">{error}</div>}
    </div>
  );
}

/** The app's own chats and settings instead of your usual ones. */
function SeparateToggle({ c, installed, busy, run }: { c: Connection; installed: Set<string>; busy: boolean; run: (l: string, f: () => Promise<unknown>, ok?: string) => void }) {
  const pair = modePair(c.app_id);
  if (!pair) return null;
  const separate = c.app_id === pair.separate;
  const target = separate ? pair.shared : pair.separate;
  // Not every mode exists everywhere (some separate modes are macOS-only).
  if (!installed.has(target)) return null;
  async function flip() {
    await api.disconnect(c.id);
    try { await api.connect(target, c.provider_id, c.model); }
    catch (e) { await api.connect(c.app_id as AppId, c.provider_id, c.model).catch(() => {}); throw e; }
  }
  const label = separate ? "Use my usual chats" : "Use separate chats";
  const help = separate ? (pair.sharedText ?? "your usual chats and settings") : (pair.separateText ?? "its own chats and settings, just for PrismFlow");
  return <button className="ghost" title={help} disabled={busy} onClick={() => run("mode", flip, "Done.")}>{label}</button>;
}
