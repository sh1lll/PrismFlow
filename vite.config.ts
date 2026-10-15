import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // Only the app's own entry; never crawl vendor-research/ (untrusted, read-only) or src-tauri/.
  optimizeDeps: { entries: ["index.html"] },
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/vendor-research/**", "**/src-tauri/**"] },
  },
  build: { target: "es2021", outDir: "dist" },
});
