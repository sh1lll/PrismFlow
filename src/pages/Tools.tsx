import { useEffect, useState } from "react";
import { api, McpServer, ToolsView, errText } from "../api";

const blank: McpServer = { name: "", command: "", args: [], env: {}, url: "" };

function splitArgs(s: string): string[] {
  return (s.match(/"[^"]*"|\S+/g) ?? []).map((a) => a.replace(/^"|"$/g, ""));
}

/** MCP servers, shared rules and skills — set once, used by all your apps. */
export default function Tools() {
  const [v, setV] = useState<ToolsView | null>(null);
  const [servers, setServers] = useState<McpServer[]>([]);
  const [apps, setApps] = useState<Set<string>>(new Set());
  const [draft, setDraft] = useState<McpServer>(blank);
  const [argText, setArgText] = useState("");
  const [envText, setEnvText] = useState("");
  const [rules, setRules] = useState("");
  const [project, setProject] = useState("");
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const load = () => api.tools().then((t) => {
    setV(t); setServers(t.servers); setRules(t.rules);
    setApps(new Set(Object.entries(t.applied).filter(([, n]) => n.length > 0).map(([a]) => a)));
  }).catch((e) => setMsg({ tone: "err", text: errText(e) }));
  useEffect(() => { load(); }, []);
  const say = (tone: string, text: string) => setMsg({ tone, text });

  function addServer() {
    const env = Object.fromEntries(envText.split("\n").map((l) => l.split("=")).filter((p) => p.length >= 2 && p[0].trim()).map(([k, ...r]) => [k.trim(), r.join("=").trim()]));
    const s = { ...draft, args: splitArgs(argText), env };
    setServers([...servers.filter((x) => x.name !== s.name), s]);
    setDraft(blank); setArgText(""); setEnvText("");
  }
  async function apply() {
    try {
      const problems = await api.mcpApply(servers, [...apps]);
      say(problems.length ? "warn" : "ok", problems.length ? problems.join("\n") : "Done. Restart the apps (or start a new chat) to pick up the change.");
      load();
    } catch (e) { say("err", errText(e)); }
  }

  if (!v) return <div><h1>Tools</h1><div className="card muted">Loading…</div></div>;
  return (
    <div>
      <h1>Tools</h1>
      <p className="muted">Set up MCP servers, rules and skills once, and share them with all your apps.</p>

      <div className="card">
        <div className="section-title">MCP servers</div>
        <div className="small muted">Tools your AI apps can use (files, web, databases…). PrismFlow adds them to each app you tick and leaves the app's other servers alone.</div>
        {servers.length === 0 && <div className="small mt">None yet.</div>}
        {servers.map((s) => (
          <div key={s.name} className="row spread mt-s small">
            <span><b>{s.name}</b> <span className="muted">{s.url || [s.command, ...s.args].join(" ")}</span></span>
            <button className="ghost" onClick={() => setServers(servers.filter((x) => x.name !== s.name))}>Remove</button>
          </div>
        ))}
        <details className="mt">
          <summary className="small">Add a server</summary>
          <label className="small muted mt-block">Name</label>
          <input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="memory" />
          <label className="small muted mt-block">Command (for servers that run on this computer)</label>
          <input value={draft.command} onChange={(e) => setDraft({ ...draft, command: e.target.value, url: "" })} placeholder="npx" />
          <input className="mt-s" value={argText} onChange={(e) => setArgText(e.target.value)} placeholder='-y @modelcontextprotocol/server-memory' />
          <label className="small muted mt-block">…or a web address</label>
          <input value={draft.url} onChange={(e) => setDraft({ ...draft, url: e.target.value, command: "" })} placeholder="https://example.com/mcp" />
          <label className="small muted mt-block">Settings (one NAME=value per line, optional)</label>
          <textarea rows={2} value={envText} onChange={(e) => setEnvText(e.target.value)} />
          <div className="small muted">These are saved in plain text in each app's settings, like the apps do themselves.</div>
          <button className="ghost mt" disabled={!draft.name.trim() || (!draft.command.trim() && !draft.url.trim())} onClick={addServer}>Add to list</button>
        </details>
        <div className="mt small">Use these servers in:</div>
        <div className="choices">
          {v.targets.map(([id, name, present]) => (
            <label key={id} className={`small ${present ? "" : "muted"}`}>
              <input type="checkbox" className="check" checked={apps.has(id)} onChange={(e) => { const n = new Set(apps); if (e.target.checked) n.add(id); else n.delete(id); setApps(n); }} />
              {name}{present ? "" : " (not found)"}
            </label>
          ))}
        </div>
        <div className="row mt"><button className="primary" onClick={apply}>Apply</button></div>
      </div>

      <div className="card">
        <div className="section-title">Shared rules</div>
        <div className="small muted">Instructions every app follows, like "use tabs" or "explain before editing". Saved into Claude Code, Codex and Gemini CLI's own rules files, or into one project.</div>
        <textarea rows={6} className="mt" value={rules} onChange={(e) => setRules(e.target.value)} placeholder="- Keep answers short.&#10;- Run the tests before saying it's done." />
        <div className="row mt">
          <button className="primary" onClick={() => api.rulesApply(rules, null).then((d) => say("ok", d.length ? `Saved for: ${d.join(", ")}.` : "None of those apps were found.")).catch((e) => say("err", errText(e)))}>Save for all my apps</button>
        </div>
        <div className="row mt">
          <input placeholder="Or a project folder: /Users/you/my-project" value={project} onChange={(e) => setProject(e.target.value)} />
          <button className="ghost" disabled={!project.trim()} onClick={() => api.rulesApply(rules, project).then(() => say("ok", "Saved to AGENTS.md (Claude Code reads it through CLAUDE.md).")).catch((e) => say("err", errText(e)))}>Save to project</button>
        </div>
      </div>

      <div className="card">
        <div className="section-title">Skills</div>
        <div className="small muted">Skills are folders of instructions an app can use. Copy one to the apps that don't have it yet.</div>
        {v.skills.length === 0 && <div className="small mt">No skills found in Claude Code or Codex.</div>}
        {v.skills.map((s) => (
          <div key={s.name} className="row spread mt-s small">
            <span><b>{s.name}</b> <span className="muted">{s.apps.join(", ")}{s.description ? ` · ${s.description.slice(0, 80)}` : ""}</span></span>
            {s.apps.length < 2 && <button className="ghost" onClick={() => api.skillShare(s.name).then((d) => { say("ok", `Copied to ${d.join(", ")}.`); load(); }).catch((e) => say("err", errText(e)))}>Share</button>}
          </div>
        ))}
      </div>
      {msg && <div className={`card small pre-wrap ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}
