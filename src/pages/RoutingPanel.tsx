import { useState } from "react";
import { api, AppId, Connection, Effort, ModelRef, Provider, Routing, Tier, errText } from "../api";

const TIERS: { tier: Tier; label: string }[] = [
  { tier: "opus", label: "Hard tasks (Opus)" },
  { tier: "sonnet", label: "Everyday tasks (Sonnet)" },
  { tier: "haiku", label: "Quick tasks (Haiku)" },
  { tier: "fable", label: "Fable" },
  { tier: "subagent", label: "Helpers (subagents)" },
];

const EFFORTS: { e: Effort; label: string }[] = [
  { e: "from_client", label: "Let the app decide" },
  { e: "off", label: "Off" },
  { e: "low", label: "Low" },
  { e: "medium", label: "Medium" },
  { e: "high", label: "High" },
  { e: "xhigh", label: "Very high" },
  { e: "max", label: "Maximum" },
];

function RefPicker({ value, onChange, providers, placeholder }: {
  value: ModelRef | undefined; onChange: (r: ModelRef | undefined) => void; providers: Provider[]; placeholder: string;
}) {
  const pid = value?.provider_id ?? "";
  return (
    <div className="row">
      <select value={pid} onChange={(e) => onChange(e.target.value ? { provider_id: e.target.value, model: value?.model ?? "" } : undefined)}>
        <option value="">{placeholder}</option>
        {providers.map((p) => <option key={p.id} value={p.id}>{p.display_name}</option>)}
      </select>
      {pid && <input placeholder="Model name" value={value?.model ?? ""} onChange={(e) => onChange({ provider_id: pid, model: e.target.value })} />}
    </div>
  );
}

export default function RoutingPanel({ c, providers, onSaved }: { c: Connection; providers: Provider[]; onSaved: () => void }) {
  const [r, setR] = useState<Routing>(c.routing);
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const claude = c.app_id === "claude_code" || c.app_id === "claude_code_separate";
  // Claude Desktop shows Claude model names; each name uses its slot's model.
  const slots = claude || c.app_id === "claude_desktop" || c.app_id === "claude_desktop_separate";
  const tierList = claude ? TIERS : TIERS.filter((t) => t.tier !== "subagent");
  const usable = providers.filter((p) => (p.has_credential || p.key_optional) && p.routes[c.app_id as AppId]);

  const setTier = (t: Tier, ref: ModelRef | undefined) => {
    const tiers = { ...r.tiers };
    if (ref) tiers[t] = ref; else delete tiers[t];
    setR({ ...r, tiers });
  };
  const setFallback = (i: number, ref: ModelRef | undefined) => {
    const f = [...r.fallbacks];
    if (ref) f[i] = ref; else f.splice(i, 1);
    setR({ ...r, fallbacks: f });
  };

  async function save() {
    setMsg(null);
    const clean = (x: ModelRef) => x.provider_id && x.model.trim();
    const tiers = Object.fromEntries(Object.entries(r.tiers).filter(([, v]) => v && clean(v)));
    const fallbacks = r.fallbacks.filter(clean);
    setBusy(true);
    try {
      await api.setRouting(c.id, { ...r, tiers: slots ? tiers : {}, fallbacks });
      setMsg({ tone: "ok", text: "Saved. Press Test to check it." });
      onSaved();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <details className="mt">
      <summary className="small">More options</summary>

      {slots && (
        <div className="mt">
          <div className="section-title small">Use a different model for…</div>
          {!claude && <div className="small muted">Claude Desktop lists Claude names like Opus 4 and Haiku 3. Each one uses the model you pick here.</div>}
          {tierList.map(({ tier, label }) => (
            <div key={tier} className="mt-s">
              <div className="small">{label}</div>
              <RefPicker value={r.tiers[tier]} onChange={(ref) => setTier(tier, ref)} providers={usable} placeholder={`Same as main (${c.model})`} />
            </div>
          ))}
        </div>
      )}

      {claude && (
        <label className="small mt-block">
          <input type="checkbox" className="check" checked={!!r.output_filter} onChange={(e) => setR({ ...r, output_filter: e.target.checked })} />
          Shorten command output with RTK (needs RTK installed)
        </label>
      )}

      <div className="mt">
        <label className="small">Thinking effort</label>
        <select value={r.reasoning} onChange={(e) => setR({ ...r, reasoning: e.target.value as Effort })}>
          {EFFORTS.map(({ e, label }) => <option key={e} value={e}>{label}</option>)}
        </select>
      </div>

      <div className="mt">
        <div className="small">Backup models</div>
        <div className="small muted">If the main model fails, these are tried in order.</div>
        {r.fallbacks.map((f, i) => (
          <div key={i} className="mt-s"><RefPicker value={f} onChange={(ref) => setFallback(i, ref)} providers={usable} placeholder="Remove" /></div>
        ))}
        {r.fallbacks.length < 5 && (
          <button className="ghost mt-s" onClick={() => setR({ ...r, fallbacks: [...r.fallbacks, { provider_id: usable[0]?.id ?? "", model: "" }] })}>Add a backup</button>
        )}
        {r.fallbacks.length > 0 && (
          <label className="small mt-block">
            <input type="checkbox" className="check" checked={r.fallback_consent} onChange={(e) => setR({ ...r, fallback_consent: e.target.checked })} />
            I understand a failed request may be charged by more than one provider.
          </label>
        )}
      </div>

      <div className="row mt">
        <button className="primary" disabled={busy} onClick={save}>{busy ? "Saving…" : "Save"}</button>
      </div>
      {msg && <div className={`small mt pre-wrap ${msg.tone}`}>{msg.text}</div>}
    </details>
  );
}
