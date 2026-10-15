import { useEffect, useState } from "react";
import { api, errText, GatewayPhase, GatewayStatus } from "../api";

const PHASE: Record<GatewayPhase, { label: string; tone: string }> = {
  stopped: { label: "Off", tone: "muted" },
  starting: { label: "Starting", tone: "warn" },
  ready: { label: "On", tone: "ok" },
  restarting: { label: "Restarting", tone: "warn" },
  failed: { label: "Stopped with an error", tone: "err" },
};

/** PrismFlow's background service (the local gateway), in plain words. */
export default function GatewayCard() {
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const load = () => api.gatewayStatus().then((s) => { setStatus(s); setError(""); }).catch((e) => setError(errText(e)));
  useEffect(() => {
    load();
    const t = setInterval(load, 3000);
    return () => clearInterval(t);
  }, []);

  async function restart() {
    setBusy(true);
    try { setStatus(await api.restartGateway()); setError(""); }
    catch (e) { setError(errText(e)); load(); }
    finally { setBusy(false); }
  }

  if (!status) return <div className="card"><div className="section-title">Background service</div><div className="muted">{error || "Loading…"}</div></div>;
  const p = PHASE[status.phase];
  return (
    <div className="card">
      <div className="row spread">
        <div className="section-title">Background service</div>
        <span className={`badge ${p.tone}`}>{p.label}</span>
      </div>
      <div className="muted small">
        {status.phase === "stopped" ? "Turns on by itself when an app needs it." : "Keep PrismFlow open while you use your apps."}
      </div>
      {status.detail && <div className={`small mt pre-wrap ${p.tone === "err" ? "err" : "warn"}`}>{status.detail}</div>}
      {error && <div className="err small mt">{error}</div>}
      {status.phase !== "stopped" && (
        <div className="row mt">
          <button className="ghost" disabled={busy} onClick={restart}>{busy ? "Restarting…" : "Restart"}</button>
        </div>
      )}
      {status.phase !== "stopped" && (
        <details className="mt">
          <summary className="small">Details</summary>
          <div className="small muted">
            {status.url} (this computer only) · {status.requests} requests · {status.rejected} refused · {status.upstream_errors} errors · {status.shortcuts} answered locally
            {status.restarts > 0 && ` · ${status.restarts} restarts`} · {status.engine}{status.node ? ` on Node ${status.node}` : ""}
          </div>
        </details>
      )}
    </div>
  );
}
