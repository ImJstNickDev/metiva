import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "METIVA_");
  const apiOrigin = env.METIVA_API_ORIGIN || "http://127.0.0.1:8080";
  return {
    build: {
      rollupOptions: {
        input: { dashboard: "index.html", docs: "docs/index.html" },
      },
    },
    plugins: [react()],
    server: { proxy: { "/api": apiOrigin, "/p": apiOrigin } },
  };
});
