import { useEffect, useRef, useState } from "react";
import { api, CodeSession, Connection, Sandbox, errText, when } from "../api";

function Event({ e }: { e: any }) {
  const item = e.item ?? {};
  if (e.type === "item.completed" && item.type === "agent_message") return <div className="msg-agent pre-wrap">{item.text}</div>;
  if (e.type === "item.completed" && item.type === "command_execution") {
    return (
      <details className="msg-cmd">
        <summary><code>$ {item.command}</code> <span className={item.exit_code === 0 ? "ok" : "warn"}>{item.exit_code === undefined ? "" : `exit ${item.exit_code}`}</span></summary>
        <pre>{(item.aggregated_output ?? "").slice(0, 8000)}</pre>
      </details>
    );
  }
  if (e.type === "item.completed" && item.type === "file_change") {
    return <div className="msg-files small">Changed: {(item.changes ?? []).map((c: any) => `${c.kind ?? "edit"} ${c.path}`).join(", ")}</div>;
  }
  if (e.type === "item.completed" && item.type === "reasoning") return <div className="muted small pre-wrap">{item.text}</div>;
  if (e.type === "turn.failed" || e.type === "error" || (e.type === "item.completed" && item.type === "error")) {
    const m = e.error?.message ?? e.message ?? item.message;
    return m ? <div className="err small pre-wrap">{String(m)}</div> : null;
  }
  return null;
}

export default function Sessions() {
  const [list, setList] = useState<CodeSession[]>([]);
  const [conns, setConns] = useState<Connection[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [events, setEvents] = useState<any[]>([]);
  const [session, setSession] = useState<CodeSession | null>(null);
  const [connId, setConnId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [error, setError] = useState("");
  const [creating, setCreating] = useState(false);
  const [folder, setFolder] = useState("");
  const [title, setTitle] = useState("");
  const [sandbox, setSandbox] = useState<Sandbox>("read_only");
  const next = useRef(0);

  const codexConns = conns.filter((c) => c.app_id === "codex_cli");
  const refreshList = () => api.sessionList().then(setList).catch((e) => setError(errText(e)));
  useEffect(() => {
    refreshList();
    api.connections().then((c) => { setConns(c); const first = c.find((x) => x.app_id === "codex_cli"); if (first) setConnId(first.id); }).catch(() => {});
  }, []);

  useEffect(() => {
    if (!active) return;
    next.current = 0;
    setEvents([]);
    let stop = false;
    const tick = async () => {
      try {
        const page = await api.sessionEvents(active, next.current);
        if (stop) return;
        next.current = page.next;
        if (page.events.length) setEvents((ev) => [...ev, ...page.events]);
        setSession(page.session);
      } catch (e) { setError(errText(e)); }
    };
    tick();
    const t = setInterval(tick, 1000);
    return () => { stop = true; clearInterval(t); };
  }, [active]);

  const running = session ? session.turns[session.turns.length - 1]?.status === "running" : false;

  async function create() {
    setError("");
    try {
      const s = await api.sessionCreate(folder, sandbox, title || folder.split("/").pop() || "Session");
      setCreating(false); setFolder(""); setTitle("");
      await refreshList();
      setActive(s.id);
    } catch (e) { setError(errText(e)); }
  }

  async function send() {
    if (!active) return;
    setError("");
    try { await api.sessionSend(active, connId, prompt); setPrompt(""); } catch (e) { setError(errText(e)); }
  }

  return (
    <div>
      <div className="row spread">
        <h1>Code sessions</h1>
        <button className="ghost" onClick={() => setCreating(!creating)}>{creating ? "Cancel" : "New session"}</button>
      </div>
      <p className="muted small">Chat with Codex about a folder on your computer. You can switch models between messages.</p>
      {codexConns.length === 0 && <div className="card warn small">Add Codex on Home first.</div>}
      {error && <div className="card err small pre-wrap">{error}</div>}

      {creating && (
        <div className="card">
          <label className="small muted">Folder (full path)</label>
          <input value={folder} onChange={(e) => setFolder(e.target.value)} placeholder="/Users/you/projects/app" />
          <label className="small muted mt-block">Title</label>
          <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="What are you working on?" />
          <label className="small muted mt-block">Codex may</label>
          <select value={sandbox} onChange={(e) => setSandbox(e.target.value as Sandbox)}>
            <option value="read_only">Read the folder only</option>
            <option value="workspace_write">Read and edit files in the folder</option>
          </select>
          {sandbox === "workspace_write" && <div className="warn small">Codex can change files here. Keep a backup or use Git so you can undo.</div>}
          <div className="row mt"><button className="primary" disabled={!folder.trim()} onClick={create}>Create</button></div>
        </div>
      )}

      <div className="sessions">
        <div className="session-list">
          {list.map((s) => (
            <button key={s.id} className={s.id === active ? "model active" : "model"} onClick={() => setActive(s.id)}>
              <span>{s.title || s.folder}</span>
              <span className="muted small">{s.turns.length} turns</span>
            </button>
          ))}
          {list.length === 0 && <div className="muted small">No sessions yet.</div>}
        </div>

        {session && (
          <div className="session-view">
            <div className="small muted">{session.folder} · {session.sandbox === "read_only" ? "read-only" : "can edit files"} · started {when(session.created_at)}</div>
            {session.turns.map((t, i) => {
              const end = session.turns[i + 1]?.first_event ?? events.length;
              return (
                <div key={i} className="turn">
                  <div className="msg-user"><span className="muted small">{t.model} · {t.connection_id.split(":")[1]}</span><div className="pre-wrap">{t.prompt}</div></div>
                  {events.slice(t.first_event, end).map((e, j) => <Event key={j} e={e} />)}
                  {t.status === "running" && <div className="muted small">Codex is working…</div>}
                  {t.status === "stopped" && <div className="warn small">Stopped.</div>}
                </div>
              );
            })}
            <div className="composer">
              <select value={connId} onChange={(e) => setConnId(e.target.value)}>
                {codexConns.map((c) => <option key={c.id} value={c.id}>{c.model} via {c.provider_id}</option>)}
              </select>
              <textarea rows={3} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="Ask Codex…" />
              <div className="row">
                <button className="primary" disabled={running || !prompt.trim() || !connId} onClick={send}>Send</button>
                {running && <button className="danger" onClick={() => api.sessionStop(session.id).catch((e) => setError(errText(e)))}>Stop</button>}
                {!running && <button className="ghost" onClick={() => api.sessionDelete(session.id).then(() => { setActive(null); setSession(null); refreshList(); }).catch((e) => setError(errText(e)))}>Delete session</button>}
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
