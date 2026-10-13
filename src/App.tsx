import { useState } from "react";
import Overview from "./pages/Overview";
import Providers from "./pages/Providers";
import Applications from "./pages/Applications";
import Connections from "./pages/Connections";
import Diagnostics from "./pages/Diagnostics";

type Page = "Overview" | "Providers" | "Applications" | "Connections" | "Diagnostics";
const PAGES: Page[] = ["Overview", "Providers", "Applications", "Connections", "Diagnostics"];

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
        {page === "Overview" && <Overview />}
        {page === "Providers" && <Providers />}
        {page === "Applications" && <Applications />}
        {page === "Connections" && <Connections />}
        {page === "Diagnostics" && <Diagnostics />}
      </main>
    </div>
  );
}
