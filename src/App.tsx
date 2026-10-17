import { Component, ReactNode, useEffect, useState } from "react";
import Home from "./pages/Home";
import Providers from "./pages/Providers";
import Usage from "./pages/Usage";
import More from "./pages/More";
import Settings from "./pages/Settings";
import Setup from "./pages/Setup";
import { AskHost } from "./pages/bits";
import { api, AppId } from "./api";

type Page = "Home" | "Providers" | "Usage" | "More" | "Settings";

/// Shown while the system's password store is slow to answer (usually a prompt).
const KEY_PROMPT = /Mac/i.test(navigator.userAgent)
  ? "macOS is asking to let PrismFlow use its saved keys. Enter your Mac password and click Always Allow."
  : "Waiting for your computer's password store. If it asked you to unlock it, do that now.";
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
  // Bumped on every navigation so a page that failed gets a fresh start.
  const [visit, setVisit] = useState(0);
  const go = (p: Page) => { setSetup(null); setPage(p); setVisit((v) => v + 1); };
  return (
    <div className="shell">
      <nav className="nav">
        <div className="brand"><img src="/prismflow-logo.png" alt="" />PrismFlow</div>
        {PAGES.map((p) => (
          <button key={p} className={p === page && !setup ? "nav-item active" : "nav-item"} onClick={() => go(p)}>{p}</button>
        ))}
      </nav>
      <main className="content">
        {keychain && <div className="banner">{KEY_PROMPT}</div>}
        {remoteUrl && <div className="banner">Remote access is on. <a onClick={() => go("More")}>Manage</a></div>}
        <PageGuard key={visit} onHome={() => go("Home")}>
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
        </PageGuard>
      </main>
      <AskHost />
    </div>
  );
}

/** One page failing shows a short message instead of a blank window. */
class PageGuard extends Component<{ onHome: () => void; children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null };
  static getDerivedStateFromError(e: unknown) { return { error: e instanceof Error ? e.message : String(e) }; }
  render() {
    if (this.state.error === null) return this.props.children;
    return (
      <div className="card">
        <div className="label">This page hit a problem</div>
        <div className="small muted selectable">{this.state.error}</div>
        <button className="primary mt" onClick={this.props.onHome}>Back to Home</button>
      </div>
    );
  }
}
