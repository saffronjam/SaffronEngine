import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    strictPort: true,
    port: 1420,
    watch: {
      // Rust's incremental build dir contains special tmp files that trigger EINVAL
      // on Linux when chokidar/fs.watch tries to watch them.
      ignored: ["**/shell/target/**"],
    },
  },
  envPrefix: ["VITE_"],
  build: {
    // The CEF shell targets one known-modern webview (Chromium), so build for the newest
    // syntax with no down-levelling rather than pinning an ECMAScript year.
    target: "esnext",
    minify: false,
  },
});
