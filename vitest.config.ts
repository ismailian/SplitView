import { defineConfig } from "vitest/config";

// The tested modules (layout tree, keymap, config) are pure, so a plain node
// environment is enough — no jsdom required.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
