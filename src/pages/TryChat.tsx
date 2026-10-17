import { useEffect, useState } from "react";
import { api, ModelInfo, Provider, errText } from "../api";

type Msg = { role: "user" | "assistant"; content: string };

/** Try any provider + model before connecting an app to it. */
export default function TryChat() {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [providerId, setProviderId] = useState("");
  const [model, setModel] = useState("");
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [msgs, setMsgs] = useState<Msg[]>([]);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    api.providers().then((ps) => {
      const ready = ps.filter((p) => p.has_credential || p.key_optional);
      setProviders(ready);
      if (ready[0]) setProviderId(ready[0].id);
    }).catch((e) => setError(errText(e)));
  }, []);
  useEffect(() => { setModels(null); setModel(""); if (providerId) api.discoverModels(providerId).then(setModels).catch(() => setModels([])); }, [providerId]);

  async function send() {
    const next = [...msgs, { role: "user" as const, content: text.trim() }];
    setMsgs(next); setText(""); setBusy(true); setError("");
    try {
      const reply = await api.tryChat(providerId, model, next);
      setMsgs([...next, { role: "assistant", content: reply || "(no reply)" }]);
    } catch (e) {
      // Put the message back in the box so it can be sent again.
      setMsgs(msgs); setText(next[next.length - 1].content); setError(errText(e));
    }
    finally { setBusy(false); }
  }

  return (
    <div>
      <h1>Try a model</h1>
      <p className="muted">Chat with any provider you've set up, before connecting an app to it.</p>
      <div className="card">
        {providers.length === 0 ? <div className="muted">Add a provider on the Providers page first.</div> : (
          <div className="row">
            <select value={providerId} onChange={(e) => setProviderId(e.target.value)}>
              {providers.map((p) => <option key={p.id} value={p.id}>{p.display_name}</option>)}
            </select>
            <input list="try-models" placeholder={models === null ? "Loading models…" : "Model name"} value={model} onChange={(e) => setModel(e.target.value)} />
            <datalist id="try-models">{(models ?? []).slice(0, 300).map((m) => <option key={m.id} value={m.id} />)}</datalist>
            {msgs.length > 0 && <button className="ghost" onClick={() => setMsgs([])}>New chat</button>}
          </div>
        )}
      </div>
      <div className="card chat">
        {msgs.length === 0 && <div className="muted small">Messages appear here.</div>}
        {msgs.map((m, i) => <div key={i} className={m.role === "user" ? "msg-user pre-wrap" : "msg-agent pre-wrap"}>{m.content}</div>)}
        {busy && <div className="muted small">Thinking…</div>}
        {error && <div className="err small pre-wrap">{error}</div>}
      </div>
      <div className="row">
        <textarea rows={2} placeholder="Type a message" value={text} onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && text.trim() && model.trim() && !busy) { e.preventDefault(); send(); } }} />
        <button className="primary" disabled={!text.trim() || !model.trim() || busy} onClick={send}>Send</button>
      </div>
    </div>
  );
}
