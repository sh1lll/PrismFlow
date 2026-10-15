import { useEffect, useMemo, useState } from "react";
import {
  api, Connection, ConnectPreview, ModelInfo, Provider, STATUS_LABEL, StageResult, errText, statusTone, when,
} from "../api";

const OUTCOME_TONE: Record<string, string> = { Pass: "ok", Warn: "warn", Fail: "err", Skipped: "muted" };

function Stages({ stages }: { stages: StageResult[] }) {
  return (
    <div className="stages">
      {stages.map((s) => (
        <div key={s.stage} className="stage">
          <span className={`badge ${OUTCOME_TONE[s.outcome]}`}>{s.outcome}</span>{" "}
          <b>{s.stage}. {s.name}</b>
          <div className="small pre-wrap muted">{s.detail}</div>
        </div>
      ))}
    </div>
  );
}

function NewConnection({ providers, onDone }: { providers: Provider[]; onDone: () => void }) {
  const eligible = providers.filter((p) => p.codex_direct);
  const [providerId, setProviderId] = useState(eligible.find((p) => p.has_credential)?.id ?? eligible[0]?.id ?? "");
  const [model, setModel] = useState("");
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [toolsOnly, setToolsOnly] = useState(true);
  const [filter, setFilter] = useState("");
  const [preview, setPreview] = useState<ConnectPreview | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const provider = eligible.find((p) => p.id === providerId);

  useEffect(() => { setModels(null); setPreview(null); }, [providerId]);
  useEffect(() => { setPreview(null); }, [model]);

  async function loadModels() {
    setBusy("models");
    setError("");
    try { setModels(await api.discoverModels(providerId)); }
    catch (e) { setError(errText(e)); }
    finally { setBusy(""); }
  }

  const shown = useMemo(() => {
    if (!models) return [];
    const f = filter.toLowerCase();
    return models
      .filter((m) => !toolsOnly || m.reports_tools !== false)
      .filter((m) => !f || m.id.toLowerCase().includes(f) || (m.name ?? "").toLowerCase().includes(f))
      .slice(0, 200);
  }, [models, filter, toolsOnly]);

  async function doPreview() {
    setBusy("preview");
    setError("");
    try { setPreview(await api.preview(providerId, model)); }
    catch (e) { setError(errText(e)); }
    finally { setBusy(""); }
  }

  async function doConnect() {
    setBusy("connect");
    setError("");
    try {
      await api.connect(providerId, model);
      setPreview(null);
      onDone();
    } catch (e) { setError(errText(e)); }
    finally { setBusy(""); }
  }

  if (eligible.length === 0) {
    return <div className="card muted">No provider in this build can connect to Codex directly.</div>;
  }

  return (
    <div className="card">
      <div className="section-title">Connect Codex CLI to a model</div>
      <label className="small muted">Provider</label>
      <select value={providerId} onChange={(e) => setProviderId(e.target.value)}>
        {eligible.map((p) => (
          <option key={p.id} value={p.id}>{p.display_name}{p.has_credential ? "" : " — no key yet"}</option>
        ))}
      </select>
      {provider && !provider.has_credential && <div className="warn small mt">Add a {provider.display_name} key under Providers first.</div>}

      <label className="small muted mt-block">Model</label>
      <div className="row">
        <input placeholder="Model ID, e.g. openai/gpt-oss-120b" value={model} onChange={(e) => setModel(e.target.value)} />
        <button className="ghost" disabled={busy !== ""} onClick={loadModels}>{busy === "models" ? "Loading…" : "Browse models"}</button>
      </div>

      {models && (
        <div className="picker">
          <div className="row">
            <input placeholder={`Filter ${models.length} models`} value={filter} onChange={(e) => setFilter(e.target.value)} />
            <label className="small nowrap"><input type="checkbox" className="check" checked={toolsOnly} onChange={(e) => setToolsOnly(e.target.checked)} /> Tool-capable only</label>
          </div>
          <div className="small muted">Listed models are not guaranteed to work with your key; verification checks that. Tool support here is provider-reported.</div>
          <div className="model-list">
            {shown.map((m) => (
              <button key={m.id} className={m.id === model ? "model active" : "model"} onClick={() => setModel(m.id)}>
                <span>{m.id}</span>
                <span className="muted small">
                  {m.context_length ? `${Math.round(m.context_length / 1000)}k ctx` : ""}
                  {m.reports_tools === true ? " · tools" : m.reports_tools === false ? " · no tools" : ""}
                </span>
              </button>
            ))}
            {shown.length === 0 && <div className="muted small">No models match.</div>}
          </div>
        </div>
      )}

      <div className="row mt">
        <button className="primary" disabled={!model.trim() || busy !== "" || !provider?.has_credential} onClick={doPreview}>
          {busy === "preview" ? "Preparing…" : "Review changes"}
        </button>
      </div>
      {error && <div className="err small mt pre-wrap">{error}</div>}

      {preview && (
        <div className="mt">
          <div className="section-title">Review before connecting</div>
          <div className="small">File: <code>{preview.file}</code></div>
          <ul className="small">{preview.summary.map((s, i) => <li key={i}><code>{s}</code></li>)}</ul>
          {preview.warnings.map((w, i) => <div key={i} className="warn small">• {w}</div>)}
          <details className="mt">
            <summary className="small">Show the file PrismFlow will write</summary>
            {preview.before && <><div className="small muted">Current</div><pre>{preview.before}</pre></>}
            <div className="small muted">After</div>
            <pre>{preview.after}</pre>
          </details>
          <div className="small mt">Use it with <code>{preview.launch_command}</code></div>
          <div className="row mt">
            <button className="primary" disabled={busy !== ""} onClick={doConnect}>{busy === "connect" ? "Connecting…" : "Connect"}</button>
            <button className="ghost" onClick={() => setPreview(null)}>Cancel</button>
          </div>
        </div>
      )}
    </div>
  );
}

function ConnectionCard({ c, onChange }: { c: Connection; onChange: () => void }) {
  const [busy, setBusy] = useState("");
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);

  async function run(label: string, f: () => Promise<unknown>, ok?: string) {
    setBusy(label);
    setMsg(null);
    try {
      await f();
      if (ok) setMsg({ tone: "ok", text: ok });
      onChange();
    } catch (e) { setMsg({ tone: "err", text: errText(e) }); }
    finally { setBusy(""); }
  }

  return (
    <div className="card">
      <div className="row spread">
        <div>
          <b>{c.model}</b> <span className="muted">via {c.provider_id} → Codex CLI</span>
        </div>
        <span className={`badge ${statusTone(c.status)}`}>{STATUS_LABEL[c.status]}</span>
      </div>
      <div className="small muted">
        <code>codex -p {c.profile}</code> · {c.managed_file}
      </div>
      {c.status === "Unverified" && (
        <div className="warn small mt">Written but not verified. Run verification before relying on it. Codex sessions that were already running keep their old settings until restarted.</div>
      )}
      {c.last_report && (
        <details className="mt" open={c.status !== "ConnectedAndVerified"}>
          <summary className="small">Last verification · {when(c.last_report.finished_at)}</summary>
          <Stages stages={c.last_report.stages} />
        </details>
      )}
      <div className="row mt">
        <button className="primary" disabled={busy !== ""} onClick={() => run("verify", () => api.verify(c.id))}>
          {busy === "verify" ? "Verifying… (up to a few minutes)" : "Verify"}
        </button>
        <button className="ghost" disabled={busy !== ""} onClick={() => run("launch", () => api.launch(c.id), "Opened Codex in Terminal.")}>Launch in Terminal</button>
        <button
          className="danger"
          disabled={busy !== ""}
          onClick={() => {
            if (confirm(`Disconnect? PrismFlow deletes ${c.managed_file} (a backup is kept). Your own Codex config is untouched.`)) {
              run("disconnect", () => api.disconnect(c.id));
            }
          }}
        >
          Disconnect
        </button>
      </div>
      {msg && <div className={`small mt pre-wrap ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

export default function Connections() {
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [connections, setConnections] = useState<Connection[] | null>(null);
  const [error, setError] = useState("");
  const load = () =>
    Promise.all([api.providers(), api.connections()])
      .then(([p, c]) => { setProviders(p); setConnections(c); })
      .catch((e) => setError(errText(e)));
  useEffect(() => { load(); }, []);

  return (
    <div>
      <h1>Connections</h1>
      {error && <div className="card err">{error}</div>}
      {connections?.map((c) => <ConnectionCard key={c.id} c={c} onChange={load} />)}
      {providers && <NewConnection providers={providers} onDone={load} />}
      <p className="muted small">Connecting the same provider again switches its model; the profile file is updated in place.</p>
    </div>
  );
}
