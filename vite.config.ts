import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  // vitest：只收 src/ 下的单测，避免裸跑时扫描 docs/、dist/ 等巨目录导致挂起。
  test: {
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    // Keep default and watch runs bounded on developer machines with many
    // concurrent Vitest processes; the include above remains src-only.
    maxWorkers: 2,
  },

  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
}));
