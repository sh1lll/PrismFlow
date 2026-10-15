import { useEffect, useState } from "react";
import { api, DiagEvent, errText, when } from "../api";

export default function Diagnostics() {
  const [events, setEvents] = useState<DiagEvent[] | null>(null);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const load = () => api.diagnostics().then(setEvents).catch((e) => setMsg({ tone: "err", text: errText(e) }));
  useEffect(() => { load(); }, []);

  async function exportAll() {
    try {
      const path = await api.exportDiagnostics();
      setMsg({ tone: "ok", text: `Exported to ${path}` });
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  return (
    <div>
      <div className="row spread">
        <h1>Diagnostics</h1>
        <div className="row">
          <button className="ghost" onClick={load}>Refresh</button>
          <button className="ghost" onClick={exportAll}>Export</button>
        </div>
      </div>
      <p className="muted">Local only. Messages are sanitized before storage; keys and bearer tokens are masked.</p>
      {msg && <div className={`card ${msg.tone}`}>{msg.text}</div>}
      {!events ? (
        <div className="card muted">Loading…</div>
      ) : events.length === 0 ? (
        <div className="card muted">No events yet.</div>
      ) : (
        <div className="card">
          {events.map((e, i) => (
            <div key={i} className="line">
              <span className={`badge ${e.level === "error" ? "err" : e.level === "warn" ? "warn" : "muted"}`}>{e.level}</span>{" "}
              <span className="muted small">{when(e.ts)} · {e.code}</span>
              <div className="pre-wrap">{e.message}</div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
