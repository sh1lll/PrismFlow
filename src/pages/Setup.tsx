import { useEffect, useMemo, useState } from "react";
import { api, AppDetection, AppId, Provider, TEST_SKIPPED, VerificationReport, appLabel, errText, isSeparate, shortStatus, statusTone } from "../api";
import { GuidedSetup, ModelPicker, Stages } from "./bits";
import TestRunning from "./TestRunning";

const POPULAR = ["openrouter", "anthropic", "openai", "gemini", "groq", "deepseek", "nvidia_nim", "mistral", "xai"];

type Step = "app" | "provider" | "model" | "done";

/** Add an app: pick it, pick a provider (and add its key), pick a model. */
export default function Setup({ initialApp, onDone }: { initialApp?: AppId; onDone: () => void }) {
  const [step, setStep] = useState<Step>(initialApp ? "provider" : "app");
  const [apps, setApps] = useState<AppDetection[] | null>(null);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [app, setApp] = useState<AppId | "">(initialApp ?? "");
  const [pid, setPid] = useState("");
  const [model, setModel] = useState("");
  const [connId, setConnId] = useState("");
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState("");
  const loadProviders = () => api.providers().then(setProviders).catch((e) => setError(errText(e)));
  useEffect(() => { api.applications().then(setApps).catch(() => setApps([])); loadProviders(); }, []);
  const provider = providers.find((p) => p.id === pid);
  const go = (s: Step) => { setError(""); setStep(s); };

  return (
    <div className="setup">
      <div className="row spread head">
        <h1>{step === "app" ? "Add an app" : appLabel(app)}</h1>
        <button className="ghost" disabled={connecting} onClick={onDone}>{step === "done" ? "Done" : "Cancel"}</button>
      </div>
      <Steps step={step} />
      {step === "app" && <PickApp apps={apps} onPick={(a) => { setApp(a); go("provider"); }} />}
      {step === "provider" && app && (
        <PickProvider app={app} providers={providers} onKeySaved={loadProviders}
          onPick={(p) => { setPid(p); setModel(""); go("model"); }} onBack={() => go("app")} />
      )}
      {step === "model" && provider && (
        <div className="card">
          <div className="label">Model from {provider.display_name}</div>
          <ModelPicker provider={provider} value={model} onChange={setModel} />
          <div className="row mt">
            <button className="ghost" disabled={connecting} onClick={() => go("provider")}>Back</button>
            <button className="primary" disabled={!model.trim() || connecting} onClick={async () => {
              setError(""); setConnecting(true);
              try { const c = await api.connect(app as AppId, pid, model.trim()); setConnId(c.id); go("done"); }
              catch (e) { setError(errText(e)); }
              finally { setConnecting(false); }
            }}>{connecting ? "Connecting…" : "Connect"}</button>
          </div>
        </div>
      )}
      {step === "done" && <Connected id={connId} app={app as AppId} />}
      {error && <div className="err small mt pre-wrap">{error}</div>}
    </div>
  );
}

function Steps({ step }: { step: Step }) {
  const all: [Step, string][] = [["app", "App"], ["provider", "Provider"], ["model", "Model"], ["done", "Done"]];
  const at = all.findIndex(([s]) => s === step);
  return (
    <div className="steps">
      {all.map(([s, label], i) => <span key={s} className={i < at ? "step past" : i === at ? "step now" : "step"}>{label}</span>)}
    </div>
  );
}

function PickApp({ apps, onPick }: { apps: AppDetection[] | null; onPick: (a: AppId) => void }) {
  if (!apps) return <div className="muted">Looking for apps on this computer…</div>;
  const shown = apps.filter((a) => a.installed && !isSeparate(a.app_id));
  if (shown.length === 0) return <div className="card muted">No supported apps found. Install one (like Claude Code or Codex) and come back.</div>;
  return (
    <div className="tiles">
      {shown.map((a) => <button key={a.app_id} className="tile" onClick={() => onPick(a.app_id as AppId)}>{appLabel(a.app_id)}</button>)}
    </div>
  );
}

function PickProvider({ app, providers, onPick, onKeySaved, onBack }: {
  app: AppId; providers: Provider[]; onPick: (id: string) => void; onKeySaved: () => Promise<void>; onBack: () => void;
}) {
  const [search, setSearch] = useState("");
  const [needsKey, setNeedsKey] = useState<Provider | null>(null);
  const usable = providers.filter((p) => p.routes[app] != null);
  const ready = (p: Provider) => p.has_credential || p.key_optional;
  const groups = useMemo(() => {
    const f = search.toLowerCase();
    if (f) return [{ title: "", items: usable.filter((p) => p.display_name.toLowerCase().includes(f)) }];
    return [
      { title: "Ready to use", items: usable.filter(ready) },
      { title: "Popular", items: POPULAR.map((id) => usable.find((p) => p.id === id)).filter((p): p is Provider => !!p && !ready(p)) },
    ];
  }, [providers, app, search]);
  if (needsKey) return <AddKey p={needsKey} onSaved={async () => { await onKeySaved(); onPick(needsKey.id); }} onBack={() => setNeedsKey(null)} />;
  return (
    <div>
      <input placeholder="Search all providers" value={search} onChange={(e) => setSearch(e.target.value)} />
      {groups.map((g) => g.items.length > 0 && (
        <div key={g.title}>
          {g.title && <h2>{g.title}</h2>}
          <div className="tiles">
            {g.items.map((p) => (
              <button key={p.id} className="tile" onClick={() => (ready(p) ? onPick(p.id) : setNeedsKey(p))}>
                {p.display_name}
                <span className="small muted">{p.local ? "On this computer · free" : ready(p) ? "Ready" : "Needs a key"}</span>
              </button>
            ))}
          </div>
        </div>
      ))}
      <button className="ghost mt" onClick={onBack}>Back</button>
    </div>
  );
}

/** A provider's key, right where it's needed. */
function AddKey({ p, onSaved, onBack }: { p: Provider; onSaved: () => Promise<void>; onBack: () => void }) {
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  if (p.credential_source !== "keychain") {
    return (
      <div className="card">
        <div className="label">{p.display_name}</div>
        <div className="muted">Sign in to {p.display_name} on the Providers page first.</div>
        <button className="ghost mt" onClick={onBack}>Back</button>
      </div>
    );
  }
  return (
    <div className="card">
      <div className="label">{p.display_name} key</div>
      <input autoFocus type="password" placeholder="Paste your key" value={key} onChange={(e) => setKey(e.target.value)} />
      {p.keys_url && <a className="small mt-block" href={p.keys_url} target="_blank" rel="noreferrer">Get a key</a>}
      <div className="row mt">
        <button className="ghost" onClick={onBack}>Back</button>
        <button className="primary" disabled={!key.trim() || busy} onClick={async () => {
          setBusy(true); setError("");
          try { await api.setCredential(p.id, key.trim()); await onSaved(); }
          catch (e) { setError(errText(e)); }
          finally { setBusy(false); }
        }}>{busy ? "Saving…" : "Save key"}</button>
      </div>
      {error && <div className="err small mt-s">{error}</div>}
    </div>
  );
}

/** Connected; the test runs on its own and can be skipped. */
function Connected({ id, app }: { id: string; app: AppId }) {
  const [report, setReport] = useState<VerificationReport | null>(null);
  const [testing, setTesting] = useState(true);
  const [testError, setTestError] = useState("");
  useEffect(() => {
    api.verify(id).then(setReport).catch((e) => { if (errText(e) !== TEST_SKIPPED) setTestError(errText(e)); }).finally(() => setTesting(false));
  }, [id]);
  return (
    <div className="card">
      <div className="label ok">{appLabel(app)} is connected</div>
      {testing && <TestRunning id={id} />}
      {report && (
        <>
          <div className={`small ${statusTone(report.status)}`}>{shortStatus(report.status)}</div>
          {report.status !== "ConnectedAndVerified" && <Stages stages={report.stages} />}
        </>
      )}
      {testError && <div className="small err">{testError}</div>}
      {app === "claude_desktop" && report?.status !== "ConnectedAndVerified" && <GuidedSetup id={id} />}
      <div className="small muted mt">Restart {appLabel(app)} if it's open.</div>
    </div>
  );
}
