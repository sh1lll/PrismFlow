import { useEffect, useState } from "react";
import Overview from "./pages/Overview";
import Providers from "./pages/Providers";
import Applications from "./pages/Applications";
import Connections from "./pages/Connections";
import Diagnostics from "./pages/Diagnostics";
import Settings from "./pages/Settings";
import Remote from "./pages/Remote";
import Sessions from "./pages/Sessions";
import { api } from "./api";

export type Page = "Overview" | "Providers" | "Apps" | "Connections" | "Code sessions" | "Activity" | "Remote access" | "Settings";
const PAGES: Page[] = ["Overview", "Providers", "Apps", "Connections", "Code sessions", "Activity", "Remote access", "Settings"];

export default function App() {
  const [page, setPage] = useState<Page>("Overview");
  // Remote access must be visibly on at all times (ADR-013).
  const [remoteUrl, setRemoteUrl] = useState<string | null>(null);
  const refreshRemote = () => api.companionState().then((s) => setRemoteUrl(s.enabled ? s.url : null)).catch(() => setRemoteUrl(null));
  useEffect(() => { refreshRemote(); const t = setInterval(refreshRemote, 15000); return () => clearInterval(t); }, []);
  // macOS asks before PrismFlow can read its saved keys; say so instead of looking frozen.
  const [keychain, setKeychain] = useState(false);
  useEffect(() => {
    const t = setInterval(() => api.keychainWaiting().then(setKeychain).catch(() => setKeychain(false)), 1000);
    return () => clearInterval(t);
  }, []);
  return (
    <div className="shell">
      <nav className="nav">
        <div className="brand">PrismFlow</div>
        {PAGES.map((p) => (
          <button key={p} className={p === page ? "nav-item active" : "nav-item"} onClick={() => setPage(p)}>
            {p}
          </button>
        ))}
      </nav>
      <main className="content">
        {keychain && (
          <div className="remote-banner">
            Waiting for your permission. macOS is asking whether PrismFlow can use its saved keys. Find the Keychain window, enter your Mac password, and click Always Allow.
          </div>
        )}
        {remoteUrl && (
          <div className="remote-banner">
            Remote access is ON at {remoteUrl}. <a onClick={() => setPage("Remote access")}>Manage</a>
          </div>
        )}
        {page === "Overview" && <Overview go={setPage} />}
        {page === "Providers" && <Providers />}
        {page === "Apps" && <Applications />}
        {page === "Connections" && <Connections />}
        {page === "Code sessions" && <Sessions />}
        {page === "Activity" && <Diagnostics />}
        {page === "Remote access" && <Remote onChange={refreshRemote} />}
        {page === "Settings" && <Settings />}
      </main>
    </div>
  );
}
