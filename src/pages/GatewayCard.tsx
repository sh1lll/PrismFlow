import { useEffect, useState } from "react";
import { api, errText, GatewayPhase, GatewayStatus } from "../api";

const PHASE: Record<GatewayPhase, { label: string; tone: string }> = {
  stopped: { label: "Stopped", tone: "muted" },
  starting: { label: "Starting", tone: "warn" },
  ready: { label: "Running", tone: "ok" },
  restarting: { label: "Restarting", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
};

/** Live status of the local gateway (broker + translation engine). */
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

  if (!status) return <div className="card"><div className="section-title">Gateway</div><div className="muted">{error || "Loading…"}</div></div>;
  const p = PHASE[status.phase];
  return (
    <div className="card">
      <div className="row spread">
        <div className="section-title">Gateway</div>
        <span className={`badge ${p.tone}`}>{p.label}</span>
      </div>
      {status.phase === "stopped" ? (
        <div className="muted small">Not running. It starts automatically when a connection needs it (Claude Code, or Codex with a Chat-only provider).</div>
      ) : (
        <>
          <div className="small">
            <code>{status.url}</code> · loopback only · {status.connections} connection{status.connections === 1 ? "" : "s"}
            {status.upstreams.length > 0 && <> · {status.upstreams.join(", ")}</>}
          </div>
          <div className="small muted">
            {status.engine}{status.node ? ` on Node ${status.node}` : ""} · {status.requests} requests · {status.rejected} refused · {status.upstream_errors} errors
            {status.restarts > 0 && ` · ${status.restarts} restart${status.restarts === 1 ? "" : "s"}`}
          </div>
        </>
      )}
      {status.detail && <div className={`small mt pre-wrap ${p.tone === "err" ? "err" : "warn"}`}>{status.detail}</div>}
      {error && <div className="err small mt">{error}</div>}
      {status.phase !== "stopped" && (
        <div className="row mt">
          <button className="ghost" disabled={busy} onClick={restart}>{busy ? "Restarting…" : "Restart gateway"}</button>
        </div>
      )}
      <div className="muted small mt">Apps hold only revocable local tokens; provider keys stay in PrismFlow. Keep PrismFlow open while you use gateway connections.</div>
    </div>
  );
}
