import { useState } from "react";
import Overview from "./pages/Overview";
import Providers from "./pages/Providers";
import Applications from "./pages/Applications";
import Connections from "./pages/Connections";
import Diagnostics from "./pages/Diagnostics";
import Settings from "./pages/Settings";

export type Page = "Overview" | "Providers" | "Applications" | "Connections" | "Diagnostics" | "Settings";
const PAGES: Page[] = ["Overview", "Providers", "Applications", "Connections", "Diagnostics", "Settings"];

export default function App() {
  const [page, setPage] = useState<Page>("Overview");
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
        {page === "Overview" && <Overview go={setPage} />}
        {page === "Providers" && <Providers />}
        {page === "Applications" && <Applications />}
        {page === "Connections" && <Connections />}
        {page === "Diagnostics" && <Diagnostics />}
        {page === "Settings" && <Settings />}
      </main>
    </div>
  );
}
