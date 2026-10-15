import { useState } from "react";
import { api, Connection, Effort, ModelRef, Provider, Routing, Tier, errText } from "../api";

const TIERS: { tier: Tier; label: string }[] = [
  { tier: "fable", label: "Fable" },
  { tier: "opus", label: "Opus" },
  { tier: "sonnet", label: "Sonnet" },
  { tier: "haiku", label: "Haiku (also background tasks)" },
  { tier: "subagent", label: "Subagents" },
];

const EFFORTS: { e: Effort; label: string }[] = [
  { e: "from_client", label: "From client" },
  { e: "off", label: "Off" },
  { e: "low", label: "Low" },
  { e: "medium", label: "Medium" },
  { e: "high", label: "High" },
  { e: "xhigh", label: "X-High" },
  { e: "max", label: "Max" },
];

/** "provider::model" text ⇄ ModelRef. "::" because model IDs themselves contain "/". */
function parseRef(text: string): ModelRef | null {
  const i = text.indexOf("::");
  if (i <= 0) return null;
  const provider_id = text.slice(0, i).trim();
  const model = text.slice(i + 2).trim();
  return provider_id && model ? { provider_id, model } : null;
}
const showRef = (r?: ModelRef) => (r ? `${r.provider_id}::${r.model}` : "");

export default function RoutingPanel({ c, providers, onSaved }: { c: Connection; providers: Provider[]; onSaved: () => void }) {
  const [r, setR] = useState<Routing>(c.routing);
  const [tierText, setTierText] = useState<Record<string, string>>(
    Object.fromEntries(TIERS.map(({ tier }) => [tier, showRef(c.routing.tiers[tier])])),
  );
  const [fallbackText, setFallbackText] = useState(c.routing.fallbacks.map(showRef).join("\n"));
  const [msg, setMsg] = useState<{ tone: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const claude = c.app_id === "claude_code";
  const usable = providers.filter((p) => (p.has_credential || p.key_optional) && p.routes[c.app_id as "codex_cli" | "claude_code"]);

  async function save() {
    setMsg(null);
    const tiers: Routing["tiers"] = {};
    for (const { tier, label } of TIERS) {
      const t = tierText[tier]?.trim();
      if (!t) continue;
      const ref = parseRef(t);
      if (!ref) return setMsg({ tone: "err", text: `${label}: write it as provider::model, e.g. groq::llama-3.3-70b-versatile` });
      tiers[tier] = ref;
    }
    const fallbacks: ModelRef[] = [];
    for (const line of fallbackText.split("\n").map((l) => l.trim()).filter(Boolean)) {
      const ref = parseRef(line);
      if (!ref) return setMsg({ tone: "err", text: `Fallback "${line}": write it as provider::model` });
      fallbacks.push(ref);
    }
    setBusy(true);
    try {
      await api.setRouting(c.id, { ...r, tiers: claude ? tiers : {}, fallbacks });
      setMsg({ tone: "ok", text: "Saved. Verify the connection again to confirm the new routes." });
      onSaved();
    } catch (e) {
      setMsg({ tone: "err", text: errText(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <details className="mt">
      <summary className="small">Routing: models per tier, reasoning, fallbacks</summary>
      <div className="small muted mt">
        Models are written as <code>provider::model</code>. Providers you can use here: {usable.map((p) => p.id).join(", ") || "none with a key yet"}.
      </div>

      {claude && (
        <div className="mt">
          <div className="section-title small">Per-tier models (empty = {c.model})</div>
          {TIERS.map(({ tier, label }) => (
            <div key={tier} className="row mt-s">
              <span className="small tier-label">{label}</span>
              <input value={tierText[tier] ?? ""} placeholder={`${c.provider_id}::${c.model}`}
                onChange={(e) => setTierText({ ...tierText, [tier]: e.target.value })} />
              <select value={r.tier_reasoning[tier] ?? "inherit"} onChange={(e) => setR({ ...r, tier_reasoning: { ...r.tier_reasoning, [tier]: e.target.value as Effort } })}>
                <option value="inherit">Reasoning: inherit</option>
                {EFFORTS.map(({ e, label }) => <option key={e} value={e}>Reasoning: {label}</option>)}
              </select>
            </div>
          ))}
        </div>
      )}

      <div className="mt">
        <label className="small muted">Reasoning policy</label>
        <select value={r.reasoning} onChange={(e) => setR({ ...r, reasoning: e.target.value as Effort })}>
          {EFFORTS.map(({ e, label }) => <option key={e} value={e}>{label}</option>)}
        </select>
        <div className="small muted">From client keeps the agent's own setting. Providers translate only the controls their API supports.</div>
      </div>

      <div className="mt">
        <label className="small muted">Fallback models, tried in order when the chosen one fails before any output (one per line)</label>
        <textarea rows={3} value={fallbackText} onChange={(e) => setFallbackText(e.target.value)} placeholder="groq::llama-3.3-70b-versatile" />
        <label className="small mt-block">
          <input type="checkbox" className="check" checked={r.fallback_consent} onChange={(e) => setR({ ...r, fallback_consent: e.target.checked })} />
          I understand one request may reach, and be billed by, several providers before it succeeds.
        </label>
      </div>

      <div className="row mt">
        <button className="primary" disabled={busy} onClick={save}>{busy ? "Saving…" : "Save routing"}</button>
      </div>
      {msg && <div className={`small mt pre-wrap ${msg.tone}`}>{msg.text}</div>}
    </details>
  );
}
