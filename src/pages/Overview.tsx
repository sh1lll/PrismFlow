import { useEffect, useState } from "react";
import type { Page } from "../App";
import { api, APP_NAME, AppDetection, Connection, DiagEvent, Provider, STATUS_LABEL, errText, providerName, statusTone, when } from "../api";
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
