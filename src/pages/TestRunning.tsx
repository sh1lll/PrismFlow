import { useEffect, useState } from "react";
import { api, STEP_NAME, StageResult } from "../api";

const MARK: Record<StageResult["outcome"], string> = { Pass: "✓", Warn: "!", Fail: "✗", Skipped: "–" };

/** Live view of a running test: finished steps, what it's waiting on, and Skip. */
export default function TestRunning({ id }: { id: string }) {
  const [done, setDone] = useState<StageResult[]>([]);
  const [secs, setSecs] = useState(0);
  useEffect(() => {
    const start = Date.now();
    const t = setInterval(() => {
      setSecs(Math.round((Date.now() - start) / 1000));
      api.verifyProgress(id).then((s) => s && setDone(s)).catch(() => {});
    }, 1000);
    return () => clearInterval(t);
  }, [id]);
  const names = done.map((s) => s.name);
  const now = names.includes("End-to-end") ? "Checking the model can use tools…"
    : names.includes("Model") || names.includes("Gateway") ? "Waiting for the model to reply…"
    : "Checking the setup…";
  return (
    <div className="mt">
      {done.map((s, i) => (
        <div key={i} className="small">{MARK[s.outcome]} {STEP_NAME[s.name] ?? s.name}</div>
      ))}
      <div className="small"><b>{now}</b> <span className="muted">{secs}s</span></div>
      {secs >= 20 && <div className="small muted mt-s">Some models take a minute or more to reply. You can skip the test and use the app now.</div>}
      <button className="ghost mt-s" onClick={() => api.cancelVerify(id)}>Skip test</button>
    </div>
  );
}
