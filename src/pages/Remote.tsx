import { useEffect, useState } from "react";
import { ask } from "./bits";
import { api, CompanionView, PairingView, errText, when } from "../api";

/** Fingerprint in readable groups of four. */
const groups = (fp: string) => fp.match(/.{1,4}/g)?.join(" ") ?? fp;

export default function Remote({ onChange }: { onChange: () => void }) {
  const [v, setV] = useState<CompanionView | null>(null);
  const [address, setAddress] = useState("");
  const [port, setPort] = useState(47822);
  const [keepOn, setKeepOn] = useState(false);
  const [pairing, setPairing] = useState<PairingView | null>(null);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);

  const apply = (nv: CompanionView) => { setV(nv); onChange(); };
  useEffect(() => {
    api.companionState().then((s) => {
      setV(s);
      setAddress(s.settings.address || s.suggested_address || "");
      setPort(s.settings.port);
      setKeepOn(s.settings.keep_on);
    }).catch((e) => setMsg({ tone: "err", text: errText(e) }));
  }, []);

  const act = (p: Promise<CompanionView>, ok?: string) =>
    p.then((nv) => { apply(nv); if (ok) setMsg({ tone: "ok", text: ok }); }).catch((e) => setMsg({ tone: "err", text: errText(e) }));

  if (!v) return <div><h1>Remote access</h1><div className="card muted">Loading…</div></div>;
  return (
    <div>
      <h1>Remote access</h1>
      <p className="muted">See and control PrismFlow from your phone. Your phone never sees your keys. Off by default.</p>

      <div className={`card ${v.enabled ? "remote-on" : ""}`}>
        <div className="row spread">
          <b>{v.enabled ? `On at ${v.url}` : "Off"}</b>
          {v.enabled && <button className="danger" onClick={() => act(api.companionDisable(), "Remote access is off.")}>Turn off</button>}
        </div>
        {!v.enabled && (
          <>
            <label className="small muted mt-block">This computer's network address</label>
            <input value={address} onChange={(e) => setAddress(e.target.value)} placeholder="192.168.1.20 or 100.x.y.z (Tailscale)" />
            <label className="small muted mt-block">Port</label>
            <input type="number" value={port} onChange={(e) => setPort(Number(e.target.value) || 47822)} />
            <label className="small mt-block"><input type="checkbox" className="check" checked={keepOn} onChange={(e) => setKeepOn(e.target.checked)} /> Turn on automatically when PrismFlow starts</label>
            <div className="row mt"><button className="primary" onClick={() => act(api.companionEnable(address, port, keepOn), "Remote access is on.")}>Turn on</button></div>
          </>
        )}
        {v.enabled && v.fingerprint && <div className="small mt">Security code: <code className="selectable">{groups(v.fingerprint)}</code></div>}
      </div>

      {v.enabled && (
        <div className="card">
          <div className="section-title">Pair a device</div>
          {!pairing ? (
            <button className="ghost" onClick={() => api.companionPair().then(setPairing).catch((e) => setMsg({ tone: "err", text: errText(e) }))}>Show pairing code</button>
          ) : (
            <div className="pair">
              <div className="qr" dangerouslySetInnerHTML={{ __html: pairing.qr_svg }} />
              <div className="small">
                <div>Scan with your phone's camera, or open <code className="selectable">{pairing.url}</code> and enter</div>
                <div className="code selectable">{pairing.code}</div>
                <div className="muted">Single use, valid for {Math.round(pairing.expires_in_secs / 60)} minutes.</div>
                <div className="warn mt">Your phone will say the connection isn't trusted. That's expected. To be safe, check that this code matches what your phone shows:</div>
                <code className="selectable">{groups(pairing.fingerprint)}</code>
                <div className="row mt"><button className="ghost" onClick={() => { setPairing(null); act(api.companionState()); }}>Done</button></div>
              </div>
            </div>
          )}
        </div>
      )}

      <div className="card">
        <div className="section-title">Paired devices</div>
        {v.devices.length === 0 && <div className="muted small">None.</div>}
        {v.devices.map((d) => (
          <div key={d.id} className="row spread line">
            <div>
              <b>{d.name}</b>
              <div className="muted small">Paired {when(d.created_at)}{d.last_seen ? ` · last seen ${when(d.last_seen)}` : ""}</div>
            </div>
            <div className="row">
              <label className="small nowrap"><input type="checkbox" className="check" checked={d.allow_actions}
                onChange={(e) => act(api.companionAllowActions(d.id, e.target.checked))} /> Allow actions</label>
              <button className="danger" onClick={async () => { if (await ask(`Remove ${d.name}? It will need to pair again.`, "Remove")) act(api.companionRevoke(d.id), "Device removed."); }}>Remove</button>
            </div>
          </div>
        ))}
      </div>
      {msg && <div className={`card small ${msg.tone}`}>{msg.text}</div>}
    </div>
  );
}
