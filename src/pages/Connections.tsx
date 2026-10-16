import RoutingPanel from "./RoutingPanel";
import TestRunning from "./TestRunning";
import { useEffect, useMemo, useState } from "react";
import {
  api, priceText, APP_IDS, APP_NAME, AppId, Connection, ConnectPreview, ModelInfo, Provider, STATUS_LABEL, STEP_NAME, StageResult,
  errText, launchHint, providerName, statusTone, when,
} from "../api";

const OUTCOME: Record<string, { label: string; tone: string }> = {
  Pass: { label: "OK", tone: "ok" },
  Warn: { label: "Check", tone: "warn" },
  Fail: { label: "Failed", tone: "err" },
  Skipped: { label: "Skipped", tone: "muted" },
};

function Stages({ stages }: { stages: StageResult[] }) {
  return (
    <div className="stages">
      {stages.map((s, i) => (
        <div key={i} className="stage">
          <span className={`badge ${OUTCOME[s.outcome].tone}`}>{OUTCOME[s.outcome].label}</span> <b>{STEP_NAME[s.name] ?? s.name}</b>
          <StageDetail text={s.detail} />
        </div>
      ))}
    </div>
  );
}

/** First paragraph in plain words; any technical output behind "More". */
function StageDetail({ text }: { text: string }) {
  const [head, ...rest] = text.split("\n\n");
  return (
    <div className="small muted">
      <span className="pre-wrap">{head}</span>
      {rest.length > 0 && (
        <details>
          <summary className="small">More</summary>
          <pre>{rest.join("\n\n")}</pre>
        </details>
      )}
    </div>
  );
}

/** Embedding and similar models can't chat, so they are no use to a coding app. */
const notForChat = (id: string) => /embed|rerank|whisper|tts|moderation/i.test(id);

function NewConnection({ providers, onDone }: { providers: Provider[]; onDone: () => void }) {
  const [appId, setAppId] = useState<AppId>("codex_cli");
  const eligible = providers.filter((p) => p.routes[appId] != null);
  const [providerId, setProviderId] = useState(eligible.find((p) => p.has_credential || p.key_optional)?.id ?? eligible[0]?.id ?? "");
  useEffect(() => {
    if (!eligible.some((p) => p.id === providerId)) setProviderId(eligible.find((p) => p.has_credential || p.key_optional)?.id ?? eligible[0]?.id ?? "");
  }, [appId]);
  const [model, setModel] = useState("");
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [toolsOnly, setToolsOnly] = useState(true);
  const [filter, setFilter] = useState("");
  const [preview, setPreview] = useState<ConnectPreview | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const provider = eligible.find((p) => p.id === providerId);

  useEffect(() => { setModels(null); setPreview(null); }, [providerId, appId]);
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
      .filter((m) => !notForChat(m.id))
      .filter((m) => !toolsOnly || m.coding)
      .filter((m) => !f || m.id.toLowerCase().includes(f) || (m.name ?? "").toLowerCase().includes(f))
      .slice(0, 200);
  }, [models, filter, toolsOnly]);

  async function doPreview() {
    setBusy("preview");
    setError("");
    try { setPreview(await api.preview(appId, providerId, model)); }
    catch (e) { setError(errText(e)); }
    finally { setBusy(""); }
  }

  async function doConnect() {
    setBusy("connect");
    setError("");
    try {
      await api.connect(appId, providerId, model);
      setPreview(null);
      setModel("");
      setModels(null);
      onDone();
    } catch (e) { setError(errText(e)); }
    finally { setBusy(""); }
  }

  return (
    <div className="card">
      <div className="section-title">Connect an app</div>
      <label className="small muted">App</label>
      <select value={appId} onChange={(e) => setAppId(e.target.value as AppId)}>
        {APP_IDS.map((a) => <option key={a} value={a}>{APP_NAME[a]}</option>)}
      </select>
      <label className="small muted mt-block">AI provider</label>
      <select value={providerId} onChange={(e) => setProviderId(e.target.value)}>
        {eligible.map((p) => (
          <option key={p.id} value={p.id}>{p.display_name}{p.has_credential || p.key_optional ? "" : " (needs a key)"}</option>
        ))}
      </select>
      {provider && !provider.has_credential && !provider.key_optional && <div className="warn small mt">Add a {provider.display_name} key on the Providers page first.</div>}

      <label className="small muted mt-block">Model</label>
      <div className="row">
        <input placeholder={provider?.example_model ? `e.g. ${provider.example_model}` : "Type a model name, or browse"} value={model} onChange={(e) => setModel(e.target.value)} />
        {provider?.example_model && !model && <button className="ghost" onClick={() => setModel(provider.example_model)}>Use example</button>}
        <button className="ghost" disabled={busy !== ""} onClick={loadModels}>{busy === "models" ? "Loading…" : "Browse"}</button>
      </div>

      {models && (
        <div className="picker">
          <div className="row">
            <input placeholder="Search models" value={filter} onChange={(e) => setFilter(e.target.value)} />
            <label className="small nowrap" title="Coding apps need models that can use tools"><input type="checkbox" className="check" checked={toolsOnly} onChange={(e) => setToolsOnly(e.target.checked)} /> Good for coding</label>
          </div>
          <div className="model-list">
            {shown.map((m) => (
              <button key={m.id} className={m.id === model ? "model active" : "model"} onClick={() => setModel(m.id)}>
                <span>{m.name && m.name !== m.id ? m.name : m.id}</span>
                <span className="muted small">
                  {[m.context_length ? `${Math.round(m.context_length / 1000)}k` : "", priceText(m), m.reports_tools === false ? "can't use tools" : ""].filter(Boolean).join(" · ")}
                </span>
              </button>
            ))}
            {shown.length === 0 && <div className="muted small">No models match.</div>}
          </div>
        </div>
      )}

      <div className="row mt">
        <button className="primary" disabled={!model.trim() || busy !== "" || !(provider?.has_credential || provider?.key_optional)} onClick={doPreview}>
          {busy === "preview" ? "Checking…" : "Next"}
        </button>
      </div>
      {error && <div className="err small mt pre-wrap">{error}</div>}

      {preview && (
        <div className="mt">
          <div className="section-title">Ready to connect</div>
          {preview.warnings.map((w, i) => <div key={i} className="small mt-s">• {w}</div>)}
          <details className="mt">
            <summary className="small">Details</summary>
            {preview.launch_command && <div className="small muted">How to start it: <code className="selectable">{preview.launch_command}</code></div>}
            {preview.file && <div className="small muted">File: <code className="selectable">{preview.file}</code></div>}
            <ul className="small muted">{preview.summary.map((s, i) => <li key={i}>{s}</li>)}</ul>
            {preview.after && <pre>{preview.after}</pre>}
          </details>
          <div className="row mt">
            <button className="primary" disabled={busy !== ""} onClick={doConnect}>{busy === "connect" ? "Connecting…" : "Connect"}</button>
            <button className="ghost" onClick={() => setPreview(null)}>Cancel</button>
          </div>
        </div>
      )}
    </div>
  );
}

function ConnectionCard({ c, providers, onChange }: { c: Connection; providers: Provider[]; onChange: () => void }) {
  const [busy, setBusy] = useState("");
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [running, setRunning] = useState<number[]>([]);
  useEffect(() => { api.runningSessions(c.id).then(setRunning).catch(() => setRunning([])); }, [c]);
  const app = APP_NAME[c.app_id];
  const desktop = c.app_id.endsWith("_desktop") || ["codex_desktop_separate", "vscode_separate", "zed_separate"].includes(c.app_id);

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
        <span><b>{app}</b> <span className="muted">→ {c.model} ({providerName(providers, c.provider_id)})</span></span>
        <span className={`badge ${statusTone(c.status)}`}>{STATUS_LABEL[c.status]}</span>
      </div>
      {c.status === "Unverified" && <div className="small muted mt-s">Press Test to check that it works.</div>}
      {running.length > 0 && <div className="warn small mt-s">Restart {app} to use the new settings.</div>}
      {c.last_report && (
        <details className="mt" open={c.status !== "ConnectedAndVerified"}>
          <summary className="small">Last test · {when(c.last_report.finished_at)}</summary>
          <Stages stages={c.last_report.stages} />
        </details>
      )}
      {c.app_id === "claude_desktop" && <GuidedSetup id={c.id} />}
      {(c.app_id === "zed" || c.app_id === "continue_ide") && <EditorGuide id={c.id} />}
      {c.route === "gateway" && <RoutingPanel c={c} providers={providers} onSaved={onChange} />}
      <div className="row mt">
        <button className="primary" disabled={busy !== ""} onClick={() => run("verify", () => api.verify(c.id))}>
          {busy === "verify" ? "Testing…" : "Test"}
        </button>
        {!["jetbrains", "vscode_claude", "vscode_claude_shared", "vscode_chat", "roo_code", "kilo_code", "zed", "continue_ide"].includes(c.app_id) && <button className="ghost" disabled={busy !== ""} onClick={() => run("launch", () => api.launch(c.id), `Opened ${app}.`)}>{desktop ? "Open app" : "Open in Terminal"}</button>}
        <button className="danger" disabled={busy !== ""}
          onClick={() => { if (confirm(`Disconnect ${app}? PrismFlow removes only its own settings.`)) run("disconnect", () => api.disconnect(c.id)); }}>
          Disconnect
        </button>
      </div>
      {busy === "verify" && <TestRunning id={c.id} />}
      {msg && <div className={`small mt pre-wrap ${msg.tone}`}>{msg.text}</div>}
      <details className="mt">
        <summary className="small">Details</summary>
        <div className="small muted">How to start it yourself: <code className="selectable">{launchHint(c)}</code></div>
        {c.managed_file && <div className="small muted">PrismFlow's file: <code className="selectable">{c.managed_file}</code></div>}
        <div className="small muted">{c.route === "gateway" ? "Goes through PrismFlow (keep PrismFlow open)." : "Talks to the provider directly."}</div>
      </details>
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
      {connections?.map((c) => <ConnectionCard key={c.id} c={c} providers={providers ?? []} onChange={load} />)}
      {providers && <NewConnection providers={providers} onDone={load} />}
      <p className="muted small">To switch models, connect the same app and provider again.</p>
    </div>
  );
}

function GuidedSetup({ id }: { id: string }) {
  const [info, setInfo] = useState<{ gateway_url: string; auth_scheme: string; token: string; model: string; steps: string[] } | null>(null);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  return (
    <details className="mt" open>
      <summary className="small">Finish setup in Claude Desktop</summary>
      {!info ? (
        <button className="ghost mt" onClick={() => api.guidedSetup(id).then(setInfo).catch((e) => setError(errText(e)))}>Show what to enter</button>
      ) : (
        <div className="mt small">
          <ol>{info.steps.map((s, i) => <li key={i}>{s}</li>)}</ol>
          <div>Address: <code className="selectable">{info.gateway_url}</code></div>
          <div>Model: <code className="selectable">{info.model}</code></div>
          <div className="row mt-s">
            <span>Key:</span>
            <button className="ghost" onClick={() => navigator.clipboard.writeText(info.token).then(() => setCopied(true))}>{copied ? "Copied" : "Copy"}</button>
          </div>
          <div className="muted">This key only works on this computer. Disconnect cancels it.</div>
        </div>
      )}
      {error && <div className="err small mt">{error}</div>}
    </details>
  );
}

/** Zed / Continue: the bit you add by hand (includes this connection's local key). */
function EditorGuide({ id }: { id: string }) {
  const [lines, setLines] = useState<string[] | null>(null);
  return (
    <details className="mt" onToggle={(e) => { if ((e.target as HTMLDetailsElement).open && !lines) api.editorGuide(id).then(setLines).catch(() => setLines([])); }}>
      <summary className="small">Finish in the app</summary>
      {(lines ?? []).map((l, i) => l.includes("\n") || l.startsWith("pft_") ? <pre key={i} className="selectable">{l}</pre> : <div key={i} className="small mt-s">{l}</div>)}
      <div className="small muted mt-s">This key only works on this computer and stops working when you disconnect.</div>
    </details>
  );
}
