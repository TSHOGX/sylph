import { defineConfig } from "vitest/config";
import path from "node:path";

/**
 * Unit tests for the pure logic behind the conversation canvas.
 *
 * Playwright already covers the rendered app end to end, but it cannot
 * economically explore the input space of a diff algorithm, a Markdown parser, or
 * a fuzzy ranker — each E2E case costs a browser and a backend. Two shipped bugs
 * came from exactly that gap: a shared regex whose `lastIndex` was rewound by
 * recursion, and a backreference that resolved to the wrong capture group inside
 * an alternation. Both are one assertion away in a unit test and cost a crashed
 * tab to find in a browser.
 *
 * `e2e/` is excluded because those files import `@playwright/test`, whose `test`
 * export would collide with Vitest's runner.
 */
export default defineConfig({
  test: {
    include: ["lib/**/*.test.ts", "components/**/*.test.ts"],
    exclude: ["e2e/**", "node_modules/**", ".next/**"],
    environment: "node",
  },
  resolve: {
    alias: { "@": path.resolve(__dirname) },
  },
});
