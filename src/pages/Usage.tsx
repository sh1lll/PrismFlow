import { useEffect, useMemo, useState } from "react";
import { api, APP_NAME, Provider, RuntimeSettings, UsageTotals, UsageView, errText, providerName, when } from "../api";

const money = (n: number) => (n === 0 ? "$0" : n < 0.01 ? "<$0.01" : `$${n.toFixed(2)}`);
const tokens = (n: number) => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));
const appOf = (conn: string) => APP_NAME[conn.split(":")[0]] ?? conn.split(":")[0];

function sum(list: UsageTotals[]): UsageTotals {
  return list.reduce((a, t) => ({ requests: a.requests + t.requests, errors: a.errors + t.errors, input_tokens: a.input_tokens + t.input_tokens,
    output_tokens: a.output_tokens + t.output_tokens, cost: a.cost + t.cost, unpriced: a.unpriced + t.unpriced }),
    { requests: 0, errors: 0, input_tokens: 0, output_tokens: 0, cost: 0, unpriced: 0 });
}

function daysAgo(n: number) {
  const d = new Date(Date.now() - n * 86_400_000);
  return d.toISOString().slice(0, 10);
}

export default function Usage() {
  const [range, setRange] = useState(30);
  const [v, setV] = useState<UsageView | null>(null);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [settings, setSettings] = useState<RuntimeSettings | null>(null);
  const [msg, setMsg] = useState("");
  const [onlyErrors, setOnlyErrors] = useState(false);
  const load = () => api.usage(daysAgo(range), 200).then(setV).catch((e) => setMsg(errText(e)));
  useEffect(() => { load(); const t = setInterval(load, 5000); return () => clearInterval(t); }, [range]);
  useEffect(() => { api.providers().then(setProviders).catch(() => {}); api.getSettings().then(setSettings).catch(() => {}); }, []);

  const byKey = useMemo(() => {
    const out: Record<string, UsageTotals[]> = {};
    for (const day of Object.values(v?.days ?? {})) for (const [k, t] of Object.entries(day)) (out[k] ??= []).push(t);
    return Object.fromEntries(Object.entries(out).map(([k, l]) => [k, sum(l)]));
  }, [v]);
  const total = sum(Object.entries(byKey).filter(([k]) => k.startsWith("prov:")).map(([, t]) => t));
  const daily = Object.entries(v?.days ?? {}).map(([d, m]) => [d, sum(Object.entries(m).filter(([k]) => k.startsWith("prov:")).map(([, t]) => t))] as const);
  const maxDay = Math.max(1, ...daily.map(([, t]) => t.requests));

  async function setLimit(p: string, value: string) {
    if (!settings) return;
    const n = Number(value);
    const budgets = { ...settings.budgets };
    if (!value || !(n >= 1)) delete budgets[p]; else budgets[p] = Math.round(n);
    const next = { ...settings, budgets };
    try { await api.setSettings(next); setSettings(next); setMsg("Saved."); load(); } catch (e) { setMsg(errText(e)); }
  }

  if (!v) return <div><h1>Usage</h1><div className="card muted">{msg || "Loading…"}</div></div>;
  const reqs = v.requests.filter((r) => !onlyErrors || r.status >= 400);
  return (
    <div>
      <h1>Usage</h1>
      <p className="muted">What your apps sent through PrismFlow. Costs are estimates from public prices; local models are free.</p>
      <div className="row mb">
        {[1, 7, 30].map((n) => <button key={n} className={n === range ? "primary" : "ghost"} onClick={() => setRange(n)}>{n === 1 ? "Today" : `${n} days`}</button>)}
      </div>
      <div className="card">
        <div className="row spread">
          <div><div className="small muted">Requests</div><b>{total.requests}</b>{total.errors > 0 && <span className="small err"> · {total.errors} failed</span>}</div>
          <div><div className="small muted">Tokens in / out</div><b>{tokens(total.input_tokens)} / {tokens(total.output_tokens)}</b></div>
          <div><div className="small muted">Estimated cost</div><b>{money(total.cost)}</b>{total.unpriced > 0 && <span className="small muted"> + {total.unpriced} unpriced</span>}</div>
        </div>
        {daily.length > 1 && (
          <div className="bars mt">
            {daily.map(([d, t]) => <div key={d} className="bar" title={`${d}: ${t.requests} requests, ${money(t.cost)}`} style={{ height: `${Math.max(4, (t.requests / maxDay) * 60)}px` }} />)}
          </div>
        )}
      </div>

      <h2>By provider this month</h2>
      <div className="card">
        {Object.keys(v.month).length === 0 && <div className="small muted">Nothing yet.</div>}
        {Object.entries(v.month).map(([p, m]) => (
          <div key={p} className="row spread mt-s">
            <span><b>{providerName(providers, p)}</b> <span className="muted small">{money(m.spent)}{m.limit ? ` of ${money(m.limit)}` : ""}</span></span>
            <span className="row">
              <span className="small muted">Monthly limit $</span>
              <input className="narrow" type="number" min={0} step={1} defaultValue={m.limit ?? ""} placeholder="none" onBlur={(e) => setLimit(p, e.target.value)} />
            </span>
          </div>
        ))}
        <div className="small muted mt">When a provider reaches its limit, PrismFlow stops sending it requests until next month or until you raise the limit.</div>
        {msg && <div className="small mt">{msg}</div>}
      </div>

      <h2>By app</h2>
      <div className="card">
        {Object.entries(byKey).filter(([k]) => k.startsWith("conn:")).map(([k, t]) => {
          const conn = k.slice(5);
          return <div key={k} className="row spread small mt-s"><span>{appOf(conn)} <span className="muted">({providerName(providers, conn.split(":")[1] ?? "")})</span></span><span>{t.requests} requests · {tokens(t.input_tokens + t.output_tokens)} tokens · {money(t.cost)}</span></div>;
        })}
      </div>

      <div className="row spread"><h2>Recent requests</h2><label className="small"><input type="checkbox" className="check" checked={onlyErrors} onChange={(e) => setOnlyErrors(e.target.checked)} /> Only failures</label></div>
      <div className="card">
        {reqs.length === 0 && <div className="small muted">No requests yet.</div>}
        <table className="log small">
          <tbody>
            {reqs.map((r, i) => (
              <tr key={i} className={r.status >= 400 ? "err" : ""}>
                <td className="muted">{when(r.ts)}</td>
                <td>{appOf(r.connection)}</td>
                <td>{r.model} <span className="muted">({providerName(providers, r.provider)})</span></td>
                <td>{r.status >= 400 ? r.error ?? `Failed (${r.status})` : `${(r.ms / 1000).toFixed(1)}s`}</td>
                <td className="muted">{tokens(r.input_tokens)} / {tokens(r.output_tokens)}</td>
                <td>{r.cost == null ? (r.status < 400 ? "?" : "") : money(r.cost)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
