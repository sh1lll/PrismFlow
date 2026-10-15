import { useEffect, useState } from "react";
import type { Page } from "../App";
import { api, AppDetection, Connection, DiagEvent, Provider, STATUS_LABEL, errText, statusTone, when } from "../api";

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

  const keyed = providers.filter((p) => p.has_credential);
  const codex = apps.find((a) => a.app_id === "codex_cli");

  return (
    <div>
      <h1>Overview</h1>
      {loadError && <div className="card err">{loadError}</div>}

      <div className="card">
        <div className="section-title">Connections</div>
        {connections.length === 0 ? (
          <div className="muted">
            No connections yet.{" "}
            {keyed.length === 0 ? (
              <>Start by <a onClick={() => go("Providers")}>adding a provider key</a>.</>
            ) : (
              <><a onClick={() => go("Connections")}>Connect Codex</a> to a model.</>
            )}
          </div>
        ) : (
          connections.map((c) => (
            <div key={c.id} className="row spread line">
              <div>
                <b>{c.model}</b> <span className="muted">via {c.provider_id} → Codex CLI (<code>codex -p {c.profile}</code>)</span>
              </div>
              <span className={`badge ${statusTone(c.status)}`}>{STATUS_LABEL[c.status]}</span>
            </div>
          ))
        )}
      </div>

      <div className="grid2">
        <div className="card">
          <div className="section-title">Provider keys</div>
          {keyed.length === 0 ? <div className="muted">None stored.</div> : keyed.map((p) => <div key={p.id}>{p.display_name}</div>)}
          <div className="muted small">Keys live only in the macOS Keychain.</div>
        </div>
        <div className="card">
          <div className="section-title">Codex CLI</div>
          {!codex ? (
            <div className="muted">Detecting…</div>
          ) : codex.installed ? (
            <div>
              {codex.version} <span className={codex.version_verified ? "ok" : "warn"}>{codex.version_verified ? "· verified version" : "· unverified version"}</span>
            </div>
          ) : (
            <div className="warn">Not installed</div>
          )}
          <div className="muted small">Gateway: not part of this build (needed only for Claude Code and Chat-only providers).</div>
        </div>
      </div>

      {errors.length > 0 && (
        <div className="card">
          <div className="section-title">Recent failures</div>
          {errors.map((e, i) => (
            <div key={i} className="line">
              <span className="muted small">{when(e.ts)}</span> {e.message}
            </div>
          ))}
          <a onClick={() => go("Diagnostics")}>Open diagnostics</a>
        </div>
      )}
    </div>
  );
}
