import { useEffect, useMemo, useState } from "react";
import { api, FoundKey, Protocol, Provider, UrlCheck, errText } from "../api";

/** Plain names for the API styles a custom server can speak. */
const STYLE: Record<Protocol, string> = {
  OpenAiChat: "OpenAI style (most servers)",
  OpenAiResponses: "OpenAI Responses style",
  AnthropicMessages: "Anthropic style",
};

type Msg = { tone: string; text: string } | null;

/** Providers whose address includes something of yours (a resource name, an account ID). */
const PRESETS: Record<string, { name: string; field: string; hint: string; url: (v: string) => string; gcloud?: boolean }> = {
  vertex: {
    name: "Google Vertex AI", field: "Project ID (and region, optional: my-project/us-central1)", hint: "my-project", gcloud: true,
    url: (v) => {
      const [project, region] = v.split("/");
      return region && region !== "global"
        ? `https://${region}-aiplatform.googleapis.com/v1/projects/${project}/locations/${region}/endpoints/openapi`
        : `https://aiplatform.googleapis.com/v1/projects/${project}/locations/global/endpoints/openapi`;
    },
  },
  azure: { name: "Azure OpenAI", field: "Resource name", hint: "The name before .openai.azure.com", url: (v) => `https://${v}.openai.azure.com/openai/v1` },
  cloudflare: { name: "Cloudflare Workers AI", field: "Account ID", hint: "Shown on your Cloudflare dashboard", url: (v) => `https://api.cloudflare.com/client/v4/accounts/${v}/ai/v1` },
};

function KeyEditor({ p, onChange, setMsg }: { p: Provider; onChange: () => void; setMsg: (m: Msg) => void }) {
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
      setMsg({ tone: "ok", text: "Key saved." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!confirm(`Remove your ${p.display_name} key?`)) return;
    try {
      await api.deleteCredential(p.id);
      setMsg({ tone: "ok", text: "Key removed." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  // Sign-in waits on the browser; it has its own flag so pasting a key
  // instead stays possible meanwhile.
  const [signingIn, setSigningIn] = useState(false);
  async function signIn() {
    setSigningIn(true);
    setMsg({ tone: "muted", text: "Finish signing in in your browser…" });
    try {
      await api.openrouterSignIn();
      setMsg({ tone: "ok", text: "Signed in. Key saved." });
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    } finally {
      setSigningIn(false);
    }
  }

  const [adding, setAdding] = useState(false);
  async function addAnother() {
    setBusy(true); setMsg(null);
    try { await api.addKey(p.id, secret); setSecret(""); setAdding(false); setMsg({ tone: "ok", text: "Key added. PrismFlow now shares requests between your keys." }); onChange(); }
    catch (e) { setMsg({ tone: "err", text: errText(e) }); }
    finally { setBusy(false); }
  }
  if (adding) {
    return (
      <div className="row">
        <input type="password" autoComplete="off" placeholder="Paste another key" value={secret} onChange={(e) => setSecret(e.target.value)} />
        <button className="primary" disabled={busy || !secret.trim()} onClick={addAnother}>Add</button>
        <button className="ghost" onClick={() => { setAdding(false); setSecret(""); }}>Cancel</button>
      </div>
    );
  }
  return editing ? (
    <div className="row">
      <input type="password" autoComplete="off" placeholder="Paste your key" value={secret} onChange={(e) => setSecret(e.target.value)} />
      <button className="primary" disabled={busy || !secret.trim()} onClick={save}>Save</button>
      <button className="ghost" onClick={() => { setEditing(false); setSecret(""); }}>Cancel</button>
    </div>
  ) : (
    <div className="row">
      {p.id === "openrouter" && !p.has_credential && <button className="primary" disabled={signingIn} onClick={signIn}>{signingIn ? "Waiting for the browser…" : "Sign in with OpenRouter"}</button>}
      <button className="ghost" onClick={() => setEditing(true)}>{p.has_credential ? "Change key" : p.key_optional ? "Add key (optional)" : p.id === "openrouter" ? "Paste a key instead" : "Add key"}</button>
      {p.has_credential && <button className="ghost" onClick={() => setAdding(true)}>Add another key</button>}
      {p.key_count > 1 && <button className="ghost" onClick={() => api.removeExtraKeys(p.id).then(() => { setMsg({ tone: "ok", text: "Extra keys removed." }); onChange(); }).catch((e) => setMsg({ tone: "err", text: errText(e) }))}>Remove extra keys</button>}
      {p.has_credential && <button className="danger" onClick={remove}>Remove key</button>}
    </div>
  );
}

const SIGNIN: Record<string, { tool: string; login: string; note: string }> = {
  gh_cli: { tool: "GitHub", login: "gh auth login", note: "Uses your GitHub account's free AI models." },
  codex_login: { tool: "ChatGPT", login: "codex login", note: "Uses your ChatGPT plan. OpenAI may stop allowing this at any time." },
  copilot_cli: { tool: "GitHub Copilot", login: "copilot login", note: "Uses your Copilot plan through GitHub's official Copilot CLI." },
  gcloud: { tool: "Google Cloud", login: "gcloud auth login", note: "Uses your Google Cloud sign-in. Usage is charged to your Google Cloud project." },
};

function SignIn({ p, onChange, setMsg }: { p: Provider; onChange: () => void; setMsg: (m: Msg) => void }) {
  const info = SIGNIN[p.credential_source];
  const set = (on: boolean) => api.setSigninConsent(p.id, on)
    .then(() => { setMsg({ tone: "ok", text: on ? "Turned on." : "Turned off." }); onChange(); })
    .catch((e) => setMsg({ tone: "err", text: errText(e) }));
  if (!p.signin_available) {
    return <div className="small muted">Sign in first by running <code>{info.login}</code> in Terminal, then come back.</div>;
  }
  return p.has_credential ? (
    <button className="danger" onClick={() => set(false)}>Turn off</button>
  ) : (
    <div>
      <div className="small muted">{info.note}</div>
      <button className="ghost mt" onClick={() => { if (confirm(`${info.note}\n\nUse your ${info.tool} account in PrismFlow?`)) set(true); }}>Use my {info.tool} account</button>
    </div>
  );
}

function badge(p: Provider): { text: string; tone: string } {
  if (p.credential_source !== "keychain") return p.has_credential ? { text: "On", tone: "ok" } : { text: "Off", tone: "muted" };
  if (p.has_credential) return { text: p.key_count > 1 ? `Ready · ${p.key_count} keys` : "Ready", tone: "ok" };
  return p.key_optional ? { text: "No key needed", tone: "muted" } : { text: "Needs a key", tone: "muted" };
}

function LocalAddress({ p, onChange }: { p: Provider; onChange: () => void }) {
  const [addr, setAddr] = useState(p.address.replace(/\/v1$/, ""));
  const [msg, setMsg] = useState("");
  return (
    <div className="mt-s">
      <div className="small muted">Make sure it's running. Address:</div>
      <div className="row">
        <input value={addr} onChange={(e) => setAddr(e.target.value)} />
        <button className="ghost" onClick={() => api.setProviderUrl(p.id, addr).then((a) => { setMsg(`Using ${a}`); onChange(); }).catch((e) => setMsg(errText(e)))}>Save</button>
        {p.address !== p.base_url && <button className="ghost" onClick={() => api.setProviderUrl(p.id, "").then((a) => { setAddr(a.replace(/\/v1$/, "")); setMsg("Back to the usual address."); onChange(); }).catch((e) => setMsg(errText(e)))}>Reset</button>}
      </div>
      {msg && <div className="small muted">{msg}</div>}
    </div>
  );
}

function ProviderRow({ p, onChange }: { p: Provider; onChange: () => void }) {
  const [open, setOpen] = useState(false);
  const [msg, setMsg] = useState<Msg>(null);

  async function removeProvider() {
    if (!confirm(`Remove ${p.display_name}?`)) return;
    try {
      await api.removeCustomProvider(p.id);
      onChange();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    }
  }

  const b = badge(p);
  return (
    <div className="card compact">
      <button className="row spread rowbtn" onClick={() => setOpen(!open)} aria-expanded={open}>
        <b>{p.display_name}</b>
        <span className={`badge ${b.tone}`}>{b.text}</span>
      </button>
      {open && (
        <div className="mt">
          {p.keys_url && <div className="small muted selectable">Get a key at {p.keys_url}</div>}
          {p.local && <LocalAddress p={p} onChange={onChange} />}
          {p.example_model && <div className="small muted">Try model: <code className="selectable">{p.example_model}</code></div>}
          <div className="mt row">
            {p.credential_source === "keychain" ? <KeyEditor p={p} onChange={onChange} setMsg={setMsg} /> : <SignIn p={p} onChange={onChange} setMsg={setMsg} />}
            {p.custom && <button className="danger" onClick={removeProvider}>Remove</button>}
          </div>
          {msg && <div className={`small mt ${msg.tone}`}>{msg.text}</div>}
          <details className="mt">
            <summary className="small">Details</summary>
            <div className="small muted selectable">
              Address: {p.base_url}
              {p.docs_url && <> · Docs: {p.docs_url}</>}
              {p.verified_at && <> · Checked {p.verified_at}</>}
            </div>
          </details>
        </div>
      )}
    </div>
  );
}

function ImportKeys({ onDone }: { onDone: () => void }) {
  const [found, setFound] = useState<FoundKey[] | null>(null);
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);
  async function scan() {
    setBusy(true); setMsg("");
    try {
      const f = await api.scanKeys();
      setFound(f);
      setPicked(new Set(f.map((x, i) => (x.already_saved ? -1 : i)).filter((i) => i >= 0)));
    } catch (e) { setMsg(errText(e)); } finally { setBusy(false); }
  }
  async function importPicked() {
    if (!found) return;
    setBusy(true);
    try {
      const n = await api.importKeys(found.filter((_, i) => picked.has(i)).map((f) => [f.provider_id, f.variable, f.source] as [string, string, string]));
      setMsg(`Imported ${n} key${n === 1 ? "" : "s"}.`); setFound(null); onDone();
    } catch (e) { setMsg(errText(e)); } finally { setBusy(false); }
  }
  return (
    <div className="card">
      <div className="row spread">
        <span>Find keys you already have</span>
        {!found && <button className="ghost" disabled={busy} onClick={scan}>{busy ? "Looking…" : "Look"}</button>}
      </div>
      {found && found.length === 0 && <div className="small mt">No keys found.</div>}
      {found && found.length > 0 && (
        <>
          {found.map((f, i) => (
            <label key={i} className="small mt-block">
              <input type="checkbox" className="check" disabled={f.already_saved} checked={picked.has(i)}
                onChange={(e) => { const n = new Set(picked); if (e.target.checked) n.add(i); else n.delete(i); setPicked(n); }} />
              <b>{f.provider_name}</b> <span className="muted">{f.masked} · {f.variable} · {f.source}{f.already_saved ? " · already saved" : ""}</span>
            </label>
          ))}
          <div className="row mt"><button className="primary" disabled={busy || picked.size === 0} onClick={importPicked}>Import {picked.size}</button><button className="ghost" onClick={() => setFound(null)}>Cancel</button></div>
        </>
      )}
      {msg && <div className="small mt">{msg}</div>}
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
  const [preset, setPreset] = useState("");
  const [presetValue, setPresetValue] = useState("");

  useEffect(() => { setCheck(null); setConsent(false); }, [url]);
  function choosePreset(id: string) {
    setPreset(id);
    setPresetValue("");
    setUrl("");
    setName(id ? PRESETS[id].name : "");
    setProtocol("OpenAiChat");
    setNeedsKey(true);
  }
  function setPresetField(v: string) {
    const clean = v.trim().replace(/[^A-Za-z0-9/-]/g, "");
    setPresetValue(clean);
    setUrl(clean ? PRESETS[preset].url(clean) : "");
  }

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
      await api.addCustomProvider(name, url, protocol, needsKey, consent, !!(preset && PRESETS[preset].gcloud));
      setName(""); setUrl(""); setCheck(null); setConsent(false); setNeedsKey(true); setProtocol("OpenAiChat"); setPreset(""); setPresetValue("");
      onDone();
    } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  }

  return (
    <div className="mt">
      <label className="small muted mt-block">Kind</label>
      <select value={preset} onChange={(e) => choosePreset(e.target.value)}>
        <option value="">Any server</option>
        {Object.entries(PRESETS).map(([id, p]) => <option key={id} value={id}>{p.name}</option>)}
      </select>
      {preset && (
        <>
          <label className="small muted mt-block">{PRESETS[preset].field}</label>
          <div className="row">
            <input value={presetValue} onChange={(e) => setPresetField(e.target.value)} placeholder={PRESETS[preset].hint} />
            <button className="ghost" disabled={!url || busy} onClick={runCheck}>Check</button>
          </div>
        </>
      )}
      <label className="small muted mt-block">Name</label>
      <input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Work server" />
      {!preset && (
        <>
          <label className="small muted mt-block">Address</label>
          <div className="row">
            <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://ai.example.com/v1" />
            <button className="ghost" disabled={!url.trim() || busy} onClick={runCheck}>Check</button>
          </div>
          <label className="small muted mt-block">Type</label>
          <select value={protocol} onChange={(e) => setProtocol(e.target.value as Protocol)}>
            {(Object.keys(STYLE) as Protocol[]).map((p) => <option key={p} value={p}>{STYLE[p]}</option>)}
          </select>
          <label className="small mt-block"><input type="checkbox" className="check" checked={needsKey} onChange={(e) => setNeedsKey(e.target.checked)} /> Needs a key</label>
        </>
      )}
      {check && (
        <div className="mt">
          {check.warnings.map((w, i) => <div key={i} className="warn small">{w}</div>)}
          <label className="small mt-block"><input type="checkbox" className="check" checked={consent} onChange={(e) => setConsent(e.target.checked)} /> I trust this server</label>
        </div>
      )}
      {error && <div className="err small mt pre-wrap">{error}</div>}
      <div className="row mt">
        <button className="primary" disabled={!check || !consent || !name.trim() || busy} onClick={save}>Add</button>
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
    const match = (p: Provider) => !f || p.display_name.toLowerCase().includes(f) || p.id.includes(f);
    const list = (providers ?? []).filter(match).sort((a, b) => Number(b.has_credential) - Number(a.has_credential));
    return [
      { title: "Use an account you already have", items: list.filter((p) => p.credential_source !== "keychain" && !p.custom), alwaysShow: false },
      { title: "AI providers", items: list.filter((p) => !p.local && !p.custom && p.credential_source === "keychain"), alwaysShow: false },
      { title: "Running on this computer", items: list.filter((p) => p.local && !p.custom), alwaysShow: false },
      { title: "Added by you", items: list.filter((p) => p.custom), alwaysShow: false },
    ];
  }, [providers, q]);

  return (
    <div>
      <div className="head"><h1>Providers</h1></div>
      {error && <div className="card err">{error}</div>}
      <input className="mb" placeholder="Search" value={q} onChange={(e) => setQ(e.target.value)} />
      {!providers ? (
        <div className="card muted">Loading…</div>
      ) : (
        groups.map((g) => (g.items.length > 0 || g.alwaysShow) && (
          <div key={g.title}>
            <h2>{g.title}</h2>
            {g.items.map((p) => <ProviderRow key={p.id} p={p} onChange={load} />)}
          </div>
        ))
      )}
      <h2>More</h2>
      <ImportKeys onDone={load} />
      <details className="card">
        <summary>Add a provider that isn't listed</summary>
        <AddCustom onDone={load} />
      </details>
    </div>
  );
}
