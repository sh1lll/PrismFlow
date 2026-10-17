import { useEffect, useState } from "react";
import {
  api, AppDetection, AppId, Connection, LaunchInfo, Provider, appLabel, errText, isSeparate,
  modePair, providerName, shortStatus, statusTone,
} from "../api";
import { EditorGuide, GuidedSetup, ModelPicker, Stages } from "./bits";
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
          {conns.map((c) => <AppRow key={c.id} c={c} providers={providers} installed={installed} onChange={load} />)}
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

function AppRow({ c, providers, installed, onChange }: { c: Connection; providers: Provider[]; installed: Set<string>; onChange: () => void }) {
  const [panel, setPanel] = useState<"" | "change" | "more">("");
  const [busy, setBusy] = useState("");
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [launch, setLaunch] = useState<LaunchInfo | null>(null);
  useEffect(() => { api.launchInfo(c.id).then(setLaunch).catch(() => setLaunch(null)); }, [c.id]);
  const name = appLabel(c.app_id);

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
        <span className={`small ${statusTone(c.status)}`}>{shortStatus(c.status)}</span>
        <button className="ghost" onClick={() => setPanel(panel === "change" ? "" : "change")}>Change</button>
        <button className="ghost icon" aria-label="More" onClick={() => setPanel(panel === "more" ? "" : "more")}>⋯</button>
      </div>

      {panel === "change" && (
        <ChangePanel c={c} providers={providers} onRefresh={onChange} onDone={(text) => { setPanel(""); setMsg({ tone: "ok", text }); onChange(); }} />
      )}

      {panel === "more" && (
        <div className="panel">
          <div className="row wrap">
            <button className="ghost" disabled={busy !== ""} onClick={() => run("test", () => api.verify(c.id))}>{busy === "test" ? "Testing…" : "Test"}</button>
            {launch && launch.opens !== "Nothing" && (
              <button className="ghost" disabled={busy !== ""} onClick={() => run("open", () => api.launch(c.id), `Opened ${name}.`)}>Open</button>
            )}
            <SeparateToggle c={c} installed={installed} busy={busy !== ""} run={run} />
            <button className="ghost danger-text" disabled={busy !== ""}
              onClick={() => { if (confirm(`Remove ${name} from PrismFlow?`)) run("remove", () => api.disconnect(c.id)); }}>Remove</button>
          </div>
          {busy === "test" && <TestRunning id={c.id} />}
          {c.last_report && c.status !== "ConnectedAndVerified" && busy !== "test" && <Stages stages={c.last_report.stages} />}
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
