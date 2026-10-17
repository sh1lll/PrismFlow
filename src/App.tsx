import { useEffect, useState } from "react";
import Home from "./pages/Home";
import Providers from "./pages/Providers";
import Usage from "./pages/Usage";
import More from "./pages/More";
import Settings from "./pages/Settings";
import Setup from "./pages/Setup";
import { api, AppId } from "./api";

type Page = "Home" | "Providers" | "Usage" | "More" | "Settings";
const PAGES: Page[] = ["Home", "Providers", "Usage", "More", "Settings"];

export default function App() {
  const [page, setPage] = useState<Page>("Home");
  // Adding an app opens over Home; `undefined` app = pick one first.
  const [setup, setSetup] = useState<{ app?: AppId } | null>(null);
  const [homeKey, setHomeKey] = useState(0);
  // First run: nothing connected yet → start adding an app (once).
  useEffect(() => {
    let seen = false;
    try { seen = localStorage.getItem("prismflow.setupSeen") === "1"; } catch { /* storage unavailable */ }
    if (seen) return;
    api.connections().then((c) => {
      if (c.length === 0) setSetup({});
      try { localStorage.setItem("prismflow.setupSeen", "1"); } catch { /* ignore */ }
    }).catch(() => {});
  }, []);
  // Remote access must be visibly on at all times.
  const [remoteUrl, setRemoteUrl] = useState<string | null>(null);
  const refreshRemote = () => api.companionState().then((s) => setRemoteUrl(s.enabled ? s.url : null)).catch(() => setRemoteUrl(null));
  useEffect(() => { refreshRemote(); const t = setInterval(refreshRemote, 15000); return () => clearInterval(t); }, []);
  // macOS asks before PrismFlow can read its saved keys; say so instead of looking frozen.
  const [keychain, setKeychain] = useState(false);
  useEffect(() => {
    const t = setInterval(() => api.keychainWaiting().then(setKeychain).catch(() => setKeychain(false)), 1000);
    return () => clearInterval(t);
  }, []);
  const go = (p: Page) => { setSetup(null); setPage(p); };
  return (
    <div className="shell">
      <nav className="nav">
        <div className="brand"><img src="/prismflow-logo.png" alt="" />PrismFlow</div>
        {PAGES.map((p) => (
          <button key={p} className={p === page && !setup ? "nav-item active" : "nav-item"} onClick={() => go(p)}>{p}</button>
        ))}
      </nav>
      <main className="content">
        {keychain && <div className="banner">macOS is asking to let PrismFlow use its saved keys. Enter your Mac password and click Always Allow.</div>}
        {remoteUrl && <div className="banner">Remote access is on. <a onClick={() => go("More")}>Manage</a></div>}
        {setup ? (
          <Setup initialApp={setup.app} onDone={() => { setSetup(null); setPage("Home"); setHomeKey((k) => k + 1); }} />
        ) : (
          <>
            {page === "Home" && <Home key={homeKey} onSetup={(app) => setSetup({ app })} />}
            {page === "Providers" && <Providers />}
            {page === "Usage" && <Usage />}
            {page === "More" && <More onRemoteChange={refreshRemote} />}
            {page === "Settings" && <Settings />}
          </>
        )}
      </main>
    </div>
  );
}
