import { useEffect, useMemo, useState } from "react";
import { api, APP_NAME, Protocol, Provider, UrlCheck, errText } from "../api";

const PROTOCOL_LABEL: Record<Protocol, string> = {
  OpenAiChat: "OpenAI Chat Completions",
  OpenAiResponses: "OpenAI Responses",
  AnthropicMessages: "Anthropic Messages",
};

function KeyEditor({ p, onChange, setMsg }: { p: Provider; onChange: () => void; setMsg: (m: { tone: string; text: string } | null) => void }) {
  const [editing, setEditing] = useState(false);
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);

  async function save() {
    setBusy(true);
    setMsg(null);
    try {
      await api.setCredential(p.id, secret);
      setSecret("");
      setEditing(false);
      setMsg({ tone: "ok", text: "Saved to the secure store. Connections using it will re-verify." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!confirm(`Remove the ${p.display_name} key from the secure store?`)) return;
    try {
      await api.deleteCredential(p.id);
      setMsg({ tone: "ok", text: "Key removed." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  return editing ? (
    <div className="row">
      <input type="password" autoComplete="off" placeholder={`Paste your ${p.display_name} API key`} value={secret} onChange={(e) => setSecret(e.target.value)} />
      <button className="primary" disabled={busy || !secret.trim()} onClick={save}>Save</button>
      <button className="ghost" onClick={() => { setEditing(false); setSecret(""); }}>Cancel</button>
    </div>
  ) : (
    <div className="row">
      <button className="ghost" onClick={() => setEditing(true)}>{p.has_credential ? "Replace key" : p.key_optional ? "Add key (optional)" : "Add key"}</button>
      {p.has_credential && <button className="danger" onClick={remove}>Remove key</button>}
    </div>
  );
}

function ProviderRow({ p, onChange }: { p: Provider; onChange: () => void }) {
  const [open, setOpen] = useState(false);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);

  async function removeProvider() {
    if (!confirm(`Remove ${p.display_name} and its stored key?`)) return;
    try {
      await api.removeCustomProvider(p.id);
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  const ready = p.has_credential || p.key_optional;
  return (
    <div className="card compact">
      <button className="row spread rowbtn" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span><b>{p.display_name}</b> <span className="muted small">{p.base_url}</span></span>
        <span className={`badge ${p.has_credential ? "ok" : p.key_optional ? "muted" : "muted"}`}>
          {p.has_credential ? "Key stored" : p.key_optional ? "No key needed" : "No key"}
        </span>
      </button>
      {open && (
        <div className="mt">
          <div className="small muted">
            {p.protocols.map((x) => PROTOCOL_LABEL[x]).join(" · ")}
            {p.verified_at && <> · facts verified {p.verified_at}</>}
            {p.custom && <> · added by you</>}
          </div>
          {(p.docs_url || p.keys_url) && (
            <div className="small muted selectable">
              {p.keys_url && <>Get a key: {p.keys_url} </>}
              {p.docs_url && <>· Docs: {p.docs_url}</>}
            </div>
          )}
          {(["codex_cli", "claude_code"] as const).map((app) => {
            const route = p.routes[app];
            return (
              <div key={app} className="small">
                {APP_NAME[app]}: {route === "direct" ? <span className="ok">direct connection</span> : route === "gateway" ? <span className="ok">through PrismFlow's local gateway</span> : <span className="muted">not supported</span>}
              </div>
            );
          })}
          {p.local && !ready && <div className="small muted">Start the server on this machine before connecting; verification checks it is reachable.</div>}
          <div className="mt row">
            <KeyEditor p={p} onChange={onChange} setMsg={setMsg} />
            {p.custom && <button className="danger" onClick={removeProvider}>Remove provider</button>}
          </div>
          {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
        </div>
      )}
    </div>
  );
}

function AddCustom({ onDone }: { onDone: () => void }) {
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [protocol, setProtocol] = useState<Protocol>("OpenAiChat");
  const [needsKey, setNeedsKey] = useState(true);
  const [check, setCheck] = useState<UrlCheck | null>(null);
  const [consent, setConsent] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => { setCheck(null); setConsent(false); }, [url]);

  async function runCheck() {
    setBusy(true);
    setError("");
    try { setCheck(await api.checkCustomUrl(url)); }
    catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  }

  async function save() {
    setBusy(true);
    setError("");
    try {
      await api.addCustomProvider(name, url, protocol, needsKey, consent);
      setName(""); setUrl(""); setCheck(null); setConsent(false);
      onDone();
    } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  }

  return (
    <div className="card">
      <div className="section-title">Add a custom provider</div>
      <div className="small muted">Any server that speaks the OpenAI or Anthropic API: a self-hosted vLLM, a company proxy, or another local server.</div>
      <label className="small muted mt-block">Name</label>
      <input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Team vLLM" />
      <label className="small muted mt-block">API base URL</label>
      <div className="row">
        <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://llm.example.com/v1" />
        <button className="ghost" disabled={!url.trim() || busy} onClick={runCheck}>Check URL</button>
      </div>
      <label className="small muted mt-block">Protocol</label>
      <select value={protocol} onChange={(e) => setProtocol(e.target.value as Protocol)}>
        {(Object.keys(PROTOCOL_LABEL) as Protocol[]).map((p) => <option key={p} value={p}>{PROTOCOL_LABEL[p]}</option>)}
      </select>
      <label className="small mt-block"><input type="checkbox" className="check" checked={needsKey} onChange={(e) => setNeedsKey(e.target.checked)} /> This server requires an API key</label>
      {check && (
        <div className="mt">
          {check.warnings.map((w, i) => <div key={i} className="warn small">• {w}</div>)}
          <label className="small mt-block"><input type="checkbox" className="check" checked={consent} onChange={(e) => setConsent(e.target.checked)} /> I trust {check.host} and allow PrismFlow to send requests to it</label>
        </div>
      )}
      {error && <div className="err small mt pre-wrap">{error}</div>}
      <div className="row mt">
        <button className="primary" disabled={!check || !consent || !name.trim() || busy} onClick={save}>Save provider</button>
      </div>
    </div>
  );
}

export default function Providers() {
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [error, setError] = useState("");
  const [q, setQ] = useState("");
  const load = () => api.providers().then(setProviders).catch((e) => setError(errText(e)));
  useEffect(() => { load(); }, []);

  const groups = useMemo(() => {
    const f = q.toLowerCase();
    const match = (p: Provider) => !f || p.display_name.toLowerCase().includes(f) || p.id.includes(f) || p.base_url.includes(f);
    const list = (providers ?? []).filter(match).sort((a, b) => Number(b.has_credential) - Number(a.has_credential));
    return [
      { title: "Cloud providers", items: list.filter((p) => !p.local && !p.custom) },
      { title: "On this machine", items: list.filter((p) => p.local && !p.custom) },
      { title: "Your custom providers", items: list.filter((p) => p.custom) },
    ];
  }, [providers, q]);

  return (
    <div>
      <h1>Providers</h1>
      <p className="muted">Keys go to your system's secure store and are never written to config files or logs. A saved key is never shown again.</p>
      {error && <div className="card err">{error}</div>}
      <input className="mb" placeholder="Search providers" value={q} onChange={(e) => setQ(e.target.value)} />
      {!providers ? (
        <div className="card muted">Loading…</div>
      ) : (
        groups.map((g) => (g.items.length > 0 || g.title.startsWith("Your")) && (
          <div key={g.title}>
            <h2>{g.title}</h2>
            {g.items.length === 0 && <div className="small muted mb">None yet.</div>}
            {g.items.map((p) => <ProviderRow key={p.id} p={p} onChange={load} />)}
          </div>
        ))
      )}
      <AddCustom onDone={load} />
    </div>
  );
}
