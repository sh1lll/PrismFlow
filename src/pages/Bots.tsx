import { useEffect, useState } from "react";
import { api, BotPlatform, BotSettings, BotsView, Connection, errText } from "../api";

const ids = (v: string) => v.split(/[\s,]+/).map((x) => x.trim()).filter(Boolean);

function TokenField({ platform, has, onSaved }: { platform: "telegram" | "discord"; has: boolean; onSaved: () => void }) {
  const [token, setToken] = useState("");
  const [msg, setMsg] = useState("");
  return (
    <div className="row mt-s">
      <input type="password" autoComplete="off" placeholder={has ? "Token saved — paste to change" : "Paste the bot token"} value={token} onChange={(e) => setToken(e.target.value)} />
      <button className="ghost" disabled={!token.trim()} onClick={() => api.setBotToken(platform, token).then(() => { setToken(""); setMsg("Saved."); onSaved(); }).catch((e) => setMsg(errText(e)))}>Save</button>
      {msg && <span className="small muted">{msg}</span>}
    </div>
  );
}

function Platform({ name, id, p, set, has, status, help, channels, onSaved }: {
  name: string; id: "telegram" | "discord"; p: BotPlatform; set: (p: BotPlatform) => void; has: boolean;
  status: { running: boolean; detail: string; messages: number }; help: string; channels: boolean; onSaved: () => void;
}) {
  return (
    <div className="card">
      <div className="row spread">
        <div className="section-title">{name}</div>
        <span className={`badge ${status.running ? "ok" : "muted"}`}>{status.running ? "On" : "Off"}</span>
      </div>
      {status.detail && <div className="small muted">{status.detail}{status.messages ? ` · ${status.messages} messages` : ""}</div>}
      <div className="small muted mt-s">{help}</div>
      <TokenField platform={id} has={has} onSaved={onSaved} />
      <label className="small muted mt-block">Who can use it (user IDs)</label>
      <input value={p.allowed_users.join(", ")} onChange={(e) => set({ ...p, allowed_users: ids(e.target.value) })} placeholder="e.g. 123456789" />
      {channels && (
        <>
          <label className="small muted mt-block">Server channels where it answers (channel IDs, optional)</label>
          <input value={p.allowed_channels.join(", ")} onChange={(e) => set({ ...p, allowed_channels: ids(e.target.value) })} placeholder="Leave empty for direct messages only" />
        </>
      )}
      <label className="small mt-block"><input type="checkbox" className="check" checked={p.enabled} onChange={(e) => set({ ...p, enabled: e.target.checked })} /> Turn on</label>
    </div>
  );
}

export default function Bots() {
  const [view, setView] = useState<BotsView | null>(null);
  const [s, setS] = useState<BotSettings | null>(null);
  const [conns, setConns] = useState<Connection[]>([]);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const load = () => api.bots().then((v) => { setView(v); setS((cur) => cur ?? v.settings); }).catch((e) => setMsg({ tone: "err", text: errText(e) }));
  useEffect(() => {
    load();
    api.connections().then((c) => setConns(c.filter((x) => x.app_id === "codex_cli"))).catch(() => {});
    const t = setInterval(() => api.bots().then(setView).catch(() => {}), 3000);
    return () => clearInterval(t);
  }, []);
  if (!view || !s) return <div><h1>Chat bots</h1><div className="card muted">Loading…</div></div>;

  async function save() {
    setMsg(null);
    try { await api.saveBots(s!); setMsg({ tone: "ok", text: s!.telegram.enabled || s!.discord.enabled ? "Saved. Bots are starting." : "Saved." }); load(); }
    catch (e) { setMsg({ tone: "err", text: errText(e) }); }
  }
  const v = s.voice;
  return (
    <div>
      <h1>Chat bots</h1>
      <p className="muted">Message your project from Telegram or Discord. Only the people you list can use it.</p>

      <div className="card">
        <div className="section-title">What the bots work on</div>
        <label className="small muted mt-block">Project folder</label>
        <input value={s.folder} onChange={(e) => setS({ ...s, folder: e.target.value })} placeholder="/Users/you/my-project" />
        <label className="small muted mt-block">Codex connection</label>
        <select value={s.connection_id} onChange={(e) => setS({ ...s, connection_id: e.target.value })}>
          <option value="">Pick one</option>
          {conns.map((c) => <option key={c.id} value={c.id}>{c.model} ({c.provider_id})</option>)}
        </select>
        {conns.length === 0 && <div className="small warn">Add Codex on Home first.</div>}
        <label className="small mt-block"><input type="checkbox" className="check" checked={s.allow_edits} onChange={(e) => setS({ ...s, allow_edits: e.target.checked })} /> Allow changing files (otherwise read-only)</label>
        <label className="small muted mt-block">Messages per person per minute</label>
        <input type="number" min={0} max={60} value={s.messages_per_minute} onChange={(e) => setS({ ...s, messages_per_minute: Number(e.target.value) || 0 })} />
      </div>

      <Platform name="Telegram" id="telegram" p={s.telegram} set={(p) => setS({ ...s, telegram: p })} has={view.has_telegram_token}
        status={view.status.telegram} channels={false} onSaved={load}
        help="Create a bot with @BotFather and paste its token. Your user ID is shown by @userinfobot." />
      <Platform name="Discord" id="discord" p={s.discord} set={(p) => setS({ ...s, discord: p })} has={view.has_discord_token}
        status={view.status.discord} channels={true} onSaved={load}
        help="Create a bot in Discord's Developer Portal, turn on Message Content, and paste its token. Copy IDs with Developer Mode on." />

      <div className="card">
        <div className="section-title">Voice notes</div>
        <select value={v.backend} onChange={(e) => setS({ ...s, voice: { ...v, backend: e.target.value as typeof v.backend } })}>
          <option value="off">Off</option>
          <option value="local">On this computer (private)</option>
          <option value="provider">With Groq or OpenAI</option>
        </select>
        {v.backend === "local" && (
          <>
            {(!view.whisper_installed || !view.ffmpeg_installed) && <div className="small warn mt-s">Needs whisper.cpp and ffmpeg installed (for example with Homebrew).</div>}
            <label className="small muted mt-block">Whisper model file</label>
            <input value={v.model_path} onChange={(e) => setS({ ...s, voice: { ...v, model_path: e.target.value } })} placeholder="/path/to/ggml-base.en.bin" />
          </>
        )}
        {v.backend === "provider" && (
          <div className="row mt-s">
            <select value={v.provider_id} onChange={(e) => setS({ ...s, voice: { ...v, provider_id: e.target.value } })}>
              <option value="">Pick one</option>
              <option value="groq">Groq</option>
              <option value="openai">OpenAI</option>
            </select>
            <input value={v.model} onChange={(e) => setS({ ...s, voice: { ...v, model: e.target.value } })} placeholder={v.provider_id === "openai" ? "gpt-4o-mini-transcribe" : "whisper-large-v3-turbo"} />
          </div>
        )}
        {v.backend === "provider" && <div className="small muted mt-s">Uses the key saved on the Providers page. Voice notes are sent to that provider.</div>}
      </div>

      <details className="card">
        <summary className="small">More options</summary>
        <label className="small muted mt-block">Proxy (if Telegram or Discord is blocked where you are)</label>
        <input value={s.proxy} onChange={(e) => setS({ ...s, proxy: e.target.value })} placeholder="http://127.0.0.1:8080" />
      </details>

      <div className="row mt">
        <button className="primary" onClick={save}>Save</button>
        {(view.status.telegram.running || view.status.discord.running) && <button className="ghost" onClick={() => api.stopBots().then(load)}>Stop bots</button>}
      </div>
      {msg && <div className={`small mt pre-wrap ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}
