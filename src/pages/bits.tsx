import { useEffect, useMemo, useState } from "react";
import { api, priceText, ModelInfo, Provider, StageResult, STEP_NAME, errText } from "../api";

/** Embedding and similar models can't chat, so they are no use to a coding app. */
const notForChat = (id: string) => /embed|rerank|whisper|tts|moderation|image|vision-only/i.test(id);

/** Type a model, or pick one from the provider's list. */
export function ModelPicker({ provider, value, onChange }: { provider: Provider; value: string; onChange: (m: string) => void }) {
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [error, setError] = useState("");
  // Local model names rarely say "coder"; don't hide them by default.
  const [codingOnly, setCodingOnly] = useState(!provider.local);
  const [filter, setFilter] = useState("");
  useEffect(() => {
    setModels(null); setError(""); setFilter(""); setCodingOnly(!provider.local);
    api.discoverModels(provider.id).then(setModels).catch((e) => setError(errText(e)));
  }, [provider.id]);
  const shown = useMemo(() => {
    const f = filter.toLowerCase();
    return (models ?? [])
      .filter((m) => !notForChat(m.id) && (!codingOnly || m.coding))
      .filter((m) => !f || m.id.toLowerCase().includes(f) || (m.name ?? "").toLowerCase().includes(f))
      .slice(0, 100);
  }, [models, filter, codingOnly]);
  return (
    <div className="picker">
      <input autoFocus placeholder={provider.example_model ? `Search, or type a model like ${provider.example_model}` : "Search or type a model"}
        value={filter || value} onChange={(e) => { setFilter(e.target.value); onChange(e.target.value.trim()); }} />
      {models && models.length > 0 && (
        <label className="small muted"><input type="checkbox" className="check" checked={codingOnly} onChange={(e) => setCodingOnly(e.target.checked)} /> Good for coding only</label>
      )}
      {!models && !error && <div className="small muted">Loading models…</div>}
      {error && <div className="small muted">Couldn't load the list. Type the model name instead.</div>}
      {models && models.length > 0 && value && !models.some((m) => m.id === value) && (
        <div className="small warn">"{value}" isn't in {provider.display_name}'s list. Pick one below, or check the spelling.</div>
      )}
      {models && (
        <div className="model-list">
          {shown.map((m) => (
            <button key={m.id} className={m.id === value ? "model active" : "model"} onClick={() => { onChange(m.id); setFilter(""); }}>
              <span>{m.name && m.name !== m.id ? m.name : m.id}</span>
              <span className="muted small">{[m.context_length ? `${Math.round(m.context_length / 1000)}k` : "", priceText(m)].filter(Boolean).join(" · ")}</span>
            </button>
          ))}
          {shown.length === 0 && <div className="muted small pad">No matches. You can still use what you typed.</div>}
        </div>
      )}
    </div>
  );
}

const OUTCOME: Record<string, { mark: string; tone: string }> = {
  Pass: { mark: "✓", tone: "ok" }, Warn: { mark: "!", tone: "warn" }, Fail: { mark: "✗", tone: "err" }, Skipped: { mark: "–", tone: "muted" },
};

/** A test's steps, one line each; technical output behind "More". */
export function Stages({ stages }: { stages: StageResult[] }) {
  return (
    <div className="stages">
      {stages.map((s, i) => {
        const [head, ...rest] = s.detail.split("\n\n");
        return (
          <div key={i} className="small">
            <span className={OUTCOME[s.outcome].tone}>{OUTCOME[s.outcome].mark}</span> {STEP_NAME[s.name] ?? s.name}
            {s.outcome !== "Pass" && <span className="muted"> · {head}</span>}
            {s.outcome !== "Pass" && rest.length > 0 && <details><summary className="small">More</summary><pre>{rest.join("\n\n")}</pre></details>}
          </div>
        );
      })}
    </div>
  );
}

/** Claude Desktop when PrismFlow couldn't set it up itself: what to enter. */
export function GuidedSetup({ id }: { id: string }) {
  const [info, setInfo] = useState<{ gateway_url: string; token: string; model: string; steps: string[] } | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");
  return (
    <details>
      <summary className="small">Set it up by hand</summary>
      {!info ? (
        <button className="ghost mt-s" onClick={() => api.guidedSetup(id).then(setInfo).catch((e) => setError(errText(e)))}>Show what to enter</button>
      ) : (
        <div className="small mt-s">
          <ol>{info.steps.map((s, i) => <li key={i}>{s}</li>)}</ol>
          <div>Address <code className="selectable">{info.gateway_url}</code></div>
          <div>Model <code className="selectable">{info.model}</code></div>
          <button className="ghost mt-s" onClick={() => navigator.clipboard.writeText(info.token).then(() => setCopied(true))}>{copied ? "Key copied" : "Copy key"}</button>
        </div>
      )}
      {error && <div className="err small">{error}</div>}
    </details>
  );
}

/** Zed / Continue: the bit you add by hand (includes this connection's key). */
export function EditorGuide({ id }: { id: string }) {
  const [lines, setLines] = useState<string[] | null>(null);
  return (
    <details onToggle={(e) => { if ((e.target as HTMLDetailsElement).open && !lines) api.editorGuide(id).then(setLines).catch(() => setLines([])); }}>
      <summary className="small">Finish in the app</summary>
      {(lines ?? []).map((l, i) => l.includes("\n") || l.startsWith("pft_") ? <pre key={i} className="selectable">{l}</pre> : <div key={i} className="small mt-s">{l}</div>)}
    </details>
  );
}

/**
 * "Are you sure?" inside the app. The web view's own confirm() box doesn't
 * reliably appear in the desktop app, which made those buttons do nothing.
 */
let openAsk: ((q: { text: string; yes: string; done: (ok: boolean) => void }) => void) | null = null;

export function ask(text: string, yes = "OK"): Promise<boolean> {
  return new Promise((done) => (openAsk ? openAsk({ text, yes, done }) : done(window.confirm(text))));
}

export function AskHost() {
  const [q, setQ] = useState<{ text: string; yes: string; done: (ok: boolean) => void } | null>(null);
  useEffect(() => { openAsk = setQ; return () => { openAsk = null; }; }, []);
  if (!q) return null;
  const close = (ok: boolean) => { q.done(ok); setQ(null); };
  return (
    <div className="ask-backdrop" onClick={() => close(false)}>
      <div className="card ask" role="dialog" aria-modal="true" onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => { if (e.key === "Escape") close(false); }}>
        <div className="pre-wrap">{q.text}</div>
        <div className="row mt">
          <button className="ghost" onClick={() => close(false)}>Cancel</button>
          <button className="primary" autoFocus onClick={() => close(true)}>{q.yes}</button>
        </div>
      </div>
    </div>
  );
}
