import { useEffect, useState } from "react";
import { api, errText } from "../api";

const CONTEXTS = [8192, 16384, 32768, 65536, 131072];

/** Models running on this computer with Ollama. */
export default function LocalModels() {
  const [v, setV] = useState<any>(null);
  const [name, setName] = useState("");
  const [msg, setMsg] = useState("");
  const load = () => api.ollama().then(setV).catch((e) => setMsg(errText(e)));
  useEffect(() => { load(); const t = setInterval(load, 2000); return () => clearInterval(t); }, []);
  const pull = (n: string) => api.ollamaPull(n).then(load).catch((e) => setMsg(errText(e)));
  async function setContext(c: number) {
    const s = await api.getSettings();
    await api.setSettings({ ...s, ollama_context: c });
    setMsg(`New connections and restarts use a ${c / 1024}k context.`);
    load();
  }
  if (!v) return <div><h1>Local models</h1><div className="card muted">Loading…</div></div>;
  const pulls = Object.entries(v.pulls ?? {}) as [string, any][];
  return (
    <div>
      <h1>Local models</h1>
      <p className="muted">Free AI that runs on this computer with Ollama. Nothing leaves your Mac.</p>
      {!v.running && <div className="card warn">{v.error} <a href="https://ollama.com/download" target="_blank" rel="noreferrer">Get Ollama</a></div>}

      <div className="card">
        <div className="section-title">Room for long tasks (context size)</div>
        <div className="small muted">Coding apps send long prompts. Ollama's default (4k) is too small, so PrismFlow gives local models more room. Bigger uses more memory.</div>
        <div className="choices">
          {CONTEXTS.map((c) => <button key={c} className={v.context === c ? "primary" : "ghost"} onClick={() => setContext(c)}>{c / 1024}k</button>)}
        </div>
      </div>

      {v.running && (
        <div className="card">
          <div className="section-title">Installed</div>
          {(v.installed as any[]).filter((m) => !m.prismflow_copy).map((m) => (
            <div key={m.name} className="row spread mt-s small">
              <span><b>{m.name}</b> <span className="muted">{m.parameters} · {m.size_gb.toFixed(1)} GB{m.running ? " · running now" : ""}</span></span>
              <button className="ghost" onClick={() => { if (confirm(`Delete ${m.name} from this computer?`)) api.ollamaDelete(m.name).then(load).catch((e) => setMsg(errText(e))); }}>Delete</button>
            </div>
          ))}
          {(v.installed as any[]).length === 0 && <div className="small muted">No models yet. Download one below.</div>}
          {(v.installed as any[]).some((m) => m.prismflow_copy) && <div className="small muted mt">Copies ending in -pf32k are PrismFlow's larger-context versions; they share the same files and take no extra space.</div>}
        </div>
      )}

      {v.running && (
        <div className="card">
          <div className="section-title">Download a model</div>
          {(v.suggested as any[]).map((s) => (
            <div key={s.name} className="row spread mt-s small">
              <span><b>{s.name}</b> <span className="muted">{s.about} · {s.size}</span></span>
              <button className="ghost" disabled={(v.installed as any[]).some((m) => m.name === s.name)} onClick={() => pull(s.name)}>Download</button>
            </div>
          ))}
          <div className="row mt">
            <input placeholder="Any model from ollama.com/library, e.g. llama3.2:3b" value={name} onChange={(e) => setName(e.target.value)} />
            <button className="ghost" disabled={!name.trim()} onClick={() => { pull(name); setName(""); }}>Download</button>
          </div>
          {pulls.map(([n, p]) => (
            <div key={n} className={`small mt-s ${p.error ? "err" : p.done ? "ok" : ""}`}>
              {n}: {p.error ? p.error : p.done ? "Done" : `${p.status}${p.percent != null ? ` ${p.percent.toFixed(0)}%` : ""}`}
            </div>
          ))}
        </div>
      )}
      {msg && <div className="card small">{msg}</div>}
    </div>
  );
}
