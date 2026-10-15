import { useEffect, useState } from "react";
import { api, APP_NAME, Provider, errText } from "../api";

function ProviderCard({ p, onChange }: { p: Provider; onChange: () => void }) {
  const [editing, setEditing] = useState(false);
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const custom = p.base_url === "";

  async function save() {
    setBusy(true);
    setMsg(null);
    try {
      await api.setCredential(p.id, secret);
      setSecret("");
      setEditing(false);
      setMsg({ tone: "ok", text: "Saved to the Keychain. Connections using it will re-verify." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!confirm(`Remove the ${p.display_name} key from the Keychain?`)) return;
    try {
      await api.deleteCredential(p.id);
      setMsg({ tone: "ok", text: "Key removed." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  return (
    <div className="card">
      <div className="row spread">
        <div>
          <b>{p.display_name}</b>{" "}
          {p.has_credential ? <span className="badge ok">Key stored</span> : <span className="badge muted">No key</span>}
        </div>
        <span className="muted small selectable">{p.docs_url || "—"}</span>
      </div>
      <div className="muted small">
        {custom ? "Base URL: you supply it" : p.base_url} · {p.protocols.join(", ")}
      </div>
      {(["codex_cli", "claude_code"] as const).map((app) => {
        const route = p.routes[app];
        return (
          <div key={app} className="small">
            {APP_NAME[app]}:{" "}
            {route === "direct" ? (
              <span className="ok">direct connection</span>
            ) : route === "gateway" ? (
              <span className="ok">through PrismFlow's local gateway</span>
            ) : (
              <span className="muted">custom endpoints are not supported in this build</span>
            )}
          </div>
        );
      })}

      {!custom && (
        <div className="mt">
          {editing ? (
            <div className="row">
              <input
                type="password"
                autoComplete="off"
                placeholder={`Paste your ${p.display_name} API key`}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
              />
              <button className="primary" disabled={busy || !secret.trim()} onClick={save}>Save</button>
              <button className="ghost" onClick={() => { setEditing(false); setSecret(""); }}>Cancel</button>
            </div>
          ) : (
            <div className="row">
              <button className="ghost" onClick={() => setEditing(true)}>{p.has_credential ? "Replace key" : "Add key"}</button>
              {p.has_credential && <button className="danger" onClick={remove}>Remove key</button>}
            </div>
          )}
        </div>
      )}
      {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}

export default function Providers() {
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [error, setError] = useState("");
  const load = () => api.providers().then(setProviders).catch((e) => setError(errText(e)));
  useEffect(() => { load(); }, []);

  return (
    <div>
      <h1>Providers</h1>
      <p className="muted">Keys are stored in the macOS Keychain and are never written to config files or logs. The full key is never shown again after you save it.</p>
      {error && <div className="card err">{error}</div>}
      {!providers ? <div className="card muted">Loading…</div> : providers.map((p) => <ProviderCard key={p.id} p={p} onChange={load} />)}
    </div>
  );
}
