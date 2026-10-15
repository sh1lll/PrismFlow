import { useEffect, useState } from "react";
import { api, RuntimeSettings, errText } from "../api";

const FIELDS: { key: keyof RuntimeSettings; label: string; help: string; max: number }[] = [
  { key: "rate_limit_per_minute", label: "Requests per minute, per connection", help: "0 = no limit. Over the limit, the agent gets HTTP 429 from PrismFlow.", max: 10000 },
  { key: "max_concurrency_per_provider", label: "Concurrent requests, per provider", help: "0 = no limit. Extra requests wait up to 30 s.", max: 256 },
  { key: "upstream_retries", label: "Transport retries per attempt", help: "Retries for connection-level failures before a fallback is tried.", max: 5 },
];

export default function Settings() {
  const [s, setS] = useState<RuntimeSettings | null>(null);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  useEffect(() => { api.getSettings().then(setS).catch((e) => setMsg({ tone: "err", text: errText(e) })); }, []);

  async function save() {
    if (!s) return;
    try {
      setS(await api.setSettings(s));
      setMsg({ tone: "ok", text: "Saved. Engine-side limits restart the gateway briefly if it is running." });
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  return (
    <div>
      <h1>Settings</h1>
      <div className="card">
        <div className="section-title">Gateway limits</div>
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
      <div className="card muted small">
        The gateway listens only on 127.0.0.1. Its port is fixed so agent configs stay valid; override it with the PRISMFLOW_BROKER_PORT environment variable.
      </div>
    </div>
  );
}
