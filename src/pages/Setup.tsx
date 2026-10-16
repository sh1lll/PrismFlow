import { useEffect, useMemo, useState } from "react";
import TestRunning from "./TestRunning";
import { api, modePair, MODE_PAIRS, priceText, APP_NAME, AppDetection, AppId, ConnectPreview, ModelInfo, Provider, STATUS_LABEL, VerificationReport, errText } from "../api";

const POPULAR = ["openrouter", "anthropic", "openai", "gemini", "groq", "deepseek", "nvidia_nim", "mistral", "xai"];
const notForChat = /embed|rerank|whisper|tts|moderation|image|vision-only/i;

/** First-run setup: app → provider → key or sign-in → model → connect and test. */
export default function Setup({ onDone }: { onDone: () => void }) {
  const [step, setStep] = useState(1);
  const [apps, setApps] = useState<AppDetection[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [app, setApp] = useState<AppId | "">("");
  const [pid, setPid] = useState("");
  const [secret, setSecret] = useState("");
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [model, setModel] = useState("");
  const [filter, setFilter] = useState("");
  const [preview, setPreview] = useState<ConnectPreview | null>(null);
  const [report, setReport] = useState<VerificationReport | null>(null);
  const [connId, setConnId] = useState("");
  const [skipped, setSkipped] = useState(false);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const loadProviders = () => api.providers().then(setProviders).catch((e) => setError(errText(e)));
  useEffect(() => { api.applications().then(setApps).catch(() => {}); loadProviders(); }, []);

  const p = providers.find((x) => x.id === pid);
  const usable = providers.filter((x) => app && x.routes[app as AppId]);
  const groups = useMemo(() => [
    { title: "Free, on this computer", items: usable.filter((x) => x.local && !x.custom) },
    { title: "Use an account you already have", items: usable.filter((x) => x.credential_source !== "keychain" && !x.custom) },
    { title: "Popular", items: POPULAR.map((id) => usable.find((x) => x.id === id)).filter(Boolean) as Provider[] },
  ], [providers, app]);
  const ready = p && (p.has_credential || p.key_optional);

  async function run(label: string, f: () => Promise<void>) {
    setBusy(label); setError("");
    try { await f(); } catch (e) { setError(errText(e)); } finally { setBusy(""); }
  }
  const go = (n: number) => { setError(""); setStep(n); };
  const [codingOnly, setCodingOnly] = useState(true);
  const shown = (models ?? []).filter((m) => !notForChat.test(m.id) && (!codingOnly || m.coding) && (!filter || m.id.toLowerCase().includes(filter.toLowerCase()))).slice(0, 100);

  return (
    <div>
      <h1>Set up an app</h1>
      <div className="steps small muted">Step {step} of 5</div>

      {step === 1 && (
        <div className="card">
          <div className="section-title">Which app do you want to use?</div>
          {apps.length === 0 && <div className="small muted mt-s">Looking for apps on this computer…</div>}
          <div className="choices">
            {apps.filter((a) => a.installed && !MODE_PAIRS.some((p) => p.separate === a.app_id && apps.some((b) => b.app_id === p.shared))).map((a) => {
              const pair = modePair(a.app_id);
              const picked = pair ? app === pair.shared || app === pair.separate : app === a.app_id;
              return <button key={a.app_id} className={picked ? "primary" : "ghost"} onClick={() => setApp(a.app_id as AppId)}>{pair?.name ?? APP_NAME[a.app_id] ?? a.display_name}</button>;
            })}
          </div>
          {app && modePair(app) && (() => {
            const pair = modePair(app)!;
            return (
              <div className="mt">
                <div className="small muted">How should it run?</div>
                <label className="small mt-block"><input type="radio" className="check" checked={app === pair.shared} onChange={() => setApp(pair.shared)} /> <b>Shared</b> · uses your usual chats and settings</label>
                <label className="small mt-block"><input type="radio" className="check" checked={app === pair.separate} onChange={() => setApp(pair.separate)} /> <b>Separate</b> · its own chats and settings, just for PrismFlow</label>
              </div>
            );
          })()}
          {apps.length > 0 && apps.every((a) => !a.installed) && <div className="muted small">None of the supported apps were found. Install one (for example Claude Code or Codex) and come back.</div>}
          <div className="row mt"><button className="primary" disabled={!app} onClick={() => go(2)}>Next</button><button className="ghost" onClick={onDone}>Skip</button></div>
        </div>
      )}

      {step === 2 && (
        <div className="card">
          <div className="section-title">Which AI should {APP_NAME[app] ?? "it"} use?</div>
          {groups.map((g) => g.items.length > 0 && (
            <div key={g.title} className="mt">
              <div className="small muted">{g.title}</div>
              <div className="choices">
                {g.items.map((x) => <button key={x.id} className={pid === x.id ? "primary" : "ghost"} onClick={() => setPid(x.id)}>{x.display_name}{x.has_credential ? " ✓" : ""}</button>)}
              </div>
            </div>
          ))}
          <div className="mt">
            <div className="small muted">All providers</div>
            <select value={pid} onChange={(e) => setPid(e.target.value)}>
              <option value="">Pick one</option>
              {usable.map((x) => <option key={x.id} value={x.id}>{x.display_name}</option>)}
            </select>
          </div>
          <div className="row mt"><button className="ghost" onClick={() => go(1)}>Back</button><button className="primary" disabled={!pid} onClick={() => go(ready ? 4 : 3)}>Next</button></div>
        </div>
      )}

      {step === 3 && p && (
        <div className="card">
          <div className="section-title">{p.credential_source === "keychain" ? `Add your ${p.display_name} key` : `Use your ${p.display_name} account`}</div>
          {p.credential_source === "keychain" ? (
            <>
              {p.keys_url && <div className="small muted selectable">Get a key at {p.keys_url}</div>}
              {p.id === "openrouter" && <button className="primary mt" disabled={busy !== ""} onClick={() => run("signin", async () => { await api.openrouterSignIn(); await loadProviders(); go(4); })}>Sign in with OpenRouter</button>}
              <div className="row mt">
                <input type="password" autoComplete="off" placeholder="Paste your key" value={secret} onChange={(e) => setSecret(e.target.value)} />
                <button className="primary" disabled={!secret.trim() || busy !== ""} onClick={() => run("key", async () => { await api.setCredential(p.id, secret); setSecret(""); await loadProviders(); go(4); })}>Save key</button>
              </div>
              <div className="small muted mt">Saved in your Mac's Keychain. Apps never see it.</div>
            </>
          ) : p.signin_available ? (
            <button className="primary" disabled={busy !== ""} onClick={() => run("consent", async () => { await api.setSigninConsent(p.id, true); await loadProviders(); go(4); })}>Use my {p.display_name} account</button>
          ) : (
            <div className="small">Sign in with that company's own tool first, then come back. See the Providers page for the command.</div>
          )}
          <div className="row mt"><button className="ghost" onClick={() => go(2)}>Back</button></div>
        </div>
      )}

      {step === 4 && p && (
        <div className="card">
          <div className="section-title">Pick a model</div>
          <div className="row">
            <input placeholder={p.example_model ? `e.g. ${p.example_model}` : "Type a model name, or search the list"} value={model} onChange={(e) => { setModel(e.target.value); setFilter(e.target.value); }} />
            <button className="ghost" disabled={busy !== ""} onClick={() => run("models", async () => setModels(await api.discoverModels(p.id)))}>{busy === "models" ? "Loading…" : "Show models"}</button>
          </div>
          {models && <label className="small mt-block"><input type="checkbox" className="check" checked={codingOnly} onChange={(e) => setCodingOnly(e.target.checked)} /> Only models good for coding</label>}
          {models && (
            <div className="picker">
              {shown.map((m) => (
                <button key={m.id} className={`rowbtn ${model === m.id ? "picked" : ""}`} onClick={() => setModel(m.id)}>
                  {m.id}{m.context_length ? <span className="muted small"> · {Math.round(m.context_length / 1000)}k</span> : null}{m.reports_tools ? <span className="muted small"> · tools</span> : null}{priceText(m) ? <span className="muted small"> · {priceText(m)}</span> : null}
                </button>
              ))}
            </div>
          )}
          {p.example_model && !model && <button className="ghost mt" onClick={() => setModel(p.example_model)}>Use {p.example_model}</button>}
          <div className="row mt">
            <button className="ghost" onClick={() => go(2)}>Back</button>
            <button className="primary" disabled={!model.trim() || busy !== ""} onClick={() => run("preview", async () => { setPreview(await api.preview(app as AppId, p.id, model.trim())); go(5); })}>Next</button>
          </div>
        </div>
      )}

      {step === 5 && p && preview && (
        <div className="card">
          <div className="section-title">Connect {APP_NAME[app]} to {model} ({p.display_name})</div>
          {preview.warnings.map((w, i) => <div key={i} className="small mt-s">• {w}</div>)}
          {!report && !skipped && (
            <div className="row mt">
              <button className="ghost" disabled={busy !== ""} onClick={() => go(4)}>Back</button>
              <button className="primary" disabled={busy !== ""} onClick={() => run("connect", async () => {
                const c = await api.connect(app as AppId, p.id, model.trim());
                setConnId(c.id);
                setBusy("test");
                try { setReport(await api.verify(c.id)); } catch (e) { if (errText(e).includes("skipped")) setSkipped(true); else throw e; }
              })}>{busy === "connect" ? "Connecting…" : busy === "test" ? "Testing…" : "Connect and test"}</button>
              {busy === "" && <button className="ghost" onClick={() => run("connect", async () => {
                const c = await api.connect(app as AppId, p.id, model.trim());
                setConnId(c.id); setSkipped(true);
              })}>Connect without testing</button>}
            </div>
          )}
          {busy === "test" && connId && <TestRunning id={connId} />}
          {(report || skipped) && (
            <div className="mt">
              {report ? <div className={report.status === "ConnectedAndVerified" ? "ok" : "warn"}><b>{STATUS_LABEL[report.status]}</b></div>
                : <div className="ok"><b>Connected.</b> <span className="small muted">Not tested. You can test it later on the Connections page.</span></div>}
              {report?.stages.filter((s) => s.outcome === "Fail").map((s) => <div key={s.stage} className="small err pre-wrap mt-s">{s.detail.split("\n\n")[0]}</div>)}
              {preview.launch_command && <div className="small mt">Start it with: <code className="selectable">{preview.launch_command}</code></div>}
              <div className="row mt"><button className="primary" onClick={onDone}>Done</button></div>
            </div>
          )}
        </div>
      )}
      {error && <div className="err small mt pre-wrap">{error}</div>}
    </div>
  );
}
