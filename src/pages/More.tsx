import { useEffect, useState } from "react";
import { Profiles, api, errText } from "../api";
import TryChat from "./TryChat";
import LocalModels from "./LocalModels";
import Tools from "./Tools";
import Sessions from "./Sessions";
import Bots from "./Bots";
import Remote from "./Remote";
import Diagnostics from "./Diagnostics";

const ITEMS = [
  { id: "try", title: "Try a model", text: "Chat with any model right here." },
  { id: "local", title: "Local models", text: "Download and manage models on this computer." },
  { id: "tools", title: "Tools", text: "Share MCP servers and rules across your apps." },
  { id: "sessions", title: "Code sessions", text: "Run Codex in a folder from PrismFlow." },
  { id: "bots", title: "Chat bots", text: "Use your setup from Telegram or Discord." },
  { id: "remote", title: "Remote access", text: "Check on PrismFlow from your phone." },
  { id: "activity", title: "Activity", text: "What PrismFlow did, and any problems." },
] as const;

type Item = (typeof ITEMS)[number]["id"];

/** Everything beyond the basics, one click away. */
export default function More({ onRemoteChange }: { onRemoteChange: () => void }) {
  const [open, setOpen] = useState<Item | null>(null);
  if (open) {
    return (
      <div>
        <button className="ghost back" onClick={() => setOpen(null)}>← More</button>
        {open === "try" && <TryChat />}
        {open === "local" && <LocalModels />}
        {open === "tools" && <Tools />}
        {open === "sessions" && <Sessions />}
        {open === "bots" && <Bots />}
        {open === "remote" && <Remote onChange={onRemoteChange} />}
        {open === "activity" && <Diagnostics />}
      </div>
    );
  }
  return (
    <div>
      <div className="head"><h1>More</h1></div>
      <div className="tiles">
        {ITEMS.map((i) => (
          <button key={i.id} className="tile" onClick={() => setOpen(i.id)}>
            {i.title}<span className="small muted">{i.text}</span>
          </button>
        ))}
      </div>
      <ProfilesCard onChange={() => {}} />
    </div>
  );
}

/** Save the current connections as a profile, and switch between profiles. */
export function ProfilesCard({ onChange }: { onChange: () => void }) {
  const [p, setP] = useState<Profiles | null>(null);
  const [name, setName] = useState("");
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => { api.profiles().then(setP).catch(() => {}); }, []);
  const act = (f: () => Promise<Profiles>, done: string) => {
    setBusy(true); setMsg("");
    f().then((x) => { setP(x); setMsg(done); onChange(); }).catch((e) => { setMsg(errText(e)); api.profiles().then(setP); onChange(); }).finally(() => setBusy(false));
  };
  const names = Object.keys(p?.list ?? {});
  return (
    <div className="card">
      <div className="label">Profiles</div>
      <div className="small muted">Save your current setup (like "Work") and switch back to it in one click.</div>
      {names.length > 0 && (
        <div className="choices">
          {names.map((n) => (
            <span key={n} className="row">
              <button className={p?.active === n ? "primary" : "ghost"} disabled={busy} onClick={() => act(() => api.switchProfile(n), `Switched to ${n}.`)}>{n}</button>
              <button className="ghost small" title="Delete" onClick={() => act(() => api.deleteProfile(n), "Deleted.")}>×</button>
            </span>
          ))}
        </div>
      )}
      <div className="row mt">
        <input placeholder="Profile name" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="ghost" disabled={!name.trim() || busy} onClick={() => act(() => api.saveProfile(name), `Saved "${name}".`)}>Save current</button>
      </div>
      {msg && <div className="small mt pre-wrap">{msg}</div>}
    </div>
  );
}
