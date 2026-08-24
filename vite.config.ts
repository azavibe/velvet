import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],

  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },

  optimizeDeps: {
    entries: ["index.html"],
  },

  clearScreen: false,

  server: {
    port: 1420,
    strictPort: true,

    // Use IPv4 loopback locally.
    // Preserve TAURI_DEV_HOST when Tauri explicitly provides one.
    host: host || "127.0.0.1",

    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,

    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
