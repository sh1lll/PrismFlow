import { useEffect, useState } from "react";
import type { Page } from "../App";
import { api, APP_NAME, AppDetection, Profiles, Connection, DiagEvent, Provider, STATUS_LABEL, errText, providerName, statusTone, when } from "../api";
import GatewayCard from "./GatewayCard";

export default function Overview({ go }: { go: (p: Page) => void }) {
  const [connections, setConnections] = useState<Connection[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [apps, setApps] = useState<AppDetection[]>([]);
  const [errors, setErrors] = useState<DiagEvent[]>([]);
  const [loadError, setLoadError] = useState("");

  useEffect(() => {
    Promise.all([api.connections(), api.providers(), api.applications(), api.diagnostics()])
      .then(([c, p, a, d]) => {
        setConnections(c);
        setProviders(p);
        setApps(a);
        setErrors(d.filter((e) => e.level === "error").slice(0, 3));
      })
      .catch((e) => setLoadError(errText(e)));
  }, []);

  const ready = providers.filter((p) => p.has_credential);
  const installed = apps.filter((a) => a.installed);

  return (
    <div>
      <div className="row spread"><h1>Overview</h1><button className="primary" onClick={() => go("Set up")}>Set up an app</button></div>
      {loadError && <div className="card err">{loadError}</div>}

      <div className="card">
        <div className="section-title">Your connections</div>
        {connections.length === 0 ? (
          <div className="muted">
            {ready.length === 0
              ? <>Nothing connected yet. <a onClick={() => go("Set up")}>Set up your first app</a> — it takes a minute.</>
              : <>Nothing connected yet. <a onClick={() => go("Set up")}>Set up an app</a>.</>}
          </div>
        ) : (
          connections.map((c) => (
            <div key={c.id} className="row spread line">
              <span><b>{APP_NAME[c.app_id]}</b> <span className="muted">→ {c.model} ({providerName(providers, c.provider_id)})</span></span>
              <span className={`badge ${statusTone(c.status)}`}>{STATUS_LABEL[c.status]}</span>
            </div>
          ))
        )}
      </div>

      <div className="card">
        <div className="section-title">Apps on this computer</div>
        {apps.length === 0 ? <div className="muted">Looking…</div> : (
          <div className="chips">
            {installed.map((a) => <span key={a.app_id} className="chip">{a.display_name}</span>)}
            {installed.length === 0 && <span className="muted">None of the supported apps were found.</span>}
          </div>
        )}
      </div>

      <ProfilesCard onChange={() => api.connections().then(setConnections).catch(() => {})} />

      <GatewayCard />

      {errors.length > 0 && (
        <div className="card">
          <div className="section-title">Recent problems</div>
          {errors.map((e, i) => <div key={i} className="line small"><span className="muted">{when(e.ts)}</span> {e.message}</div>)}
          <a onClick={() => go("Activity")}>See all</a>
        </div>
      )}
    </div>
  );
}

/** Save the current connections as a profile, and switch between profiles. */
function ProfilesCard({ onChange }: { onChange: () => void }) {
  const [p, setP] = useState<Profiles | null>(null);
  const [name, setName] = useState("");
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => { api.profiles().then(setP).catch(() => {}); }, []);
  const act = (f: () => Promise<Profiles>, done: string) => {
    setBusy(true); setMsg("");
    f().then((x) => { setP(x); setMsg(done); onChange(); }).catch((e) => { setMsg(errText(e)); api.profiles().then(setP); onChange(); }).finally(() => setBusy(false));
  };
  const names = Object.keys(p?.list ?? {});
  return (
    <div className="card">
      <div className="section-title">Profiles</div>
      <div className="small muted">Save your current set of connections (like "Work" or "Personal") and switch between them in one click.</div>
      {names.length > 0 && (
        <div className="choices">
          {names.map((n) => (
            <span key={n} className="row">
              <button className={p?.active === n ? "primary" : "ghost"} disabled={busy} onClick={() => act(() => api.switchProfile(n), `Switched to ${n}.`)}>{n}</button>
              <button className="ghost small" title="Delete" onClick={() => act(() => api.deleteProfile(n), "Deleted.")}>×</button>
            </span>
          ))}
        </div>
      )}
      <div className="row mt">
        <input placeholder="Profile name" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="ghost" disabled={!name.trim() || busy} onClick={() => act(() => api.saveProfile(name), `Saved "${name}".`)}>Save current</button>
      </div>
      {msg && <div className="small mt pre-wrap">{msg}</div>}
    </div>
  );
}
