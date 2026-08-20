import { defineConfig, devices } from "@playwright/test";
import path from "node:path";

/**
 * End-to-end configuration.
 *
 * Both servers are started by Playwright so a run needs no manual setup. The
 * backend gets its own data directory and a deterministic mock runner, so E2E
 * never touches the developer's real journal, real archive, or real agents.
 *
 * Ports are offset from the defaults (4317 / 3000) so a run cannot collide with
 * a development server the user already has open.
 */
const BACKEND_PORT = 4318;
const FRONTEND_PORT = 3101;
const dataDir = path.join(__dirname, ".e2e-data");

export default defineConfig({
  testDir: "./e2e",
  // Journal state is shared through one backend, so parallel workers would race
  // on the session list. Correctness matters more than wall time for four specs.
  workers: 1,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "line" : [["list"]],
  timeout: 30_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: `http://127.0.0.1:${FRONTEND_PORT}`,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      command: "cargo run --quiet",
      cwd: path.join(__dirname, "..", "backend"),
      url: `http://127.0.0.1:${BACKEND_PORT}/api/v1/health`,
      reuseExistingServer: false,
      // A cold `cargo run` compiles the whole backend first.
      timeout: 180_000,
      stdout: "pipe",
      stderr: "pipe",
      env: {
        SYLPH_WEB_PORT: String(BACKEND_PORT),
        SYLPH_WEB_DATA_DIR: dataDir,
        SYLPH_RUNNERS_DIR: path.join(__dirname, "e2e", "fixtures", "runners"),
        // Point at a path that does not exist so archive-dependent screens
        // exercise their unavailable state instead of the developer's archive.
        TRACEDB_BIN: path.join(dataDir, "absent-trace-db"),
        // Browse roots default to `$HOME`, but the specs create sessions in
        // `/tmp`. Naming both keeps the directory picker assertable without
        // depending on what happens to live in the developer's home directory.
        SYLPH_WEB_BROWSE_ROOTS: "/tmp",
      },
    },
    {
      // Build inside the webServer command, not before it. Next compiles
      // rewrites into the route manifest at build time, so SYLPH_WEB_BACKEND
      // must be set for the build; a prebuilt app would proxy to the default
      // port 4317 and silently talk to the developer's real backend.
      command: `npx next build && npx next start -p ${FRONTEND_PORT}`,
      cwd: __dirname,
      url: `http://127.0.0.1:${FRONTEND_PORT}`,
      reuseExistingServer: false,
      timeout: 180_000,
      env: { SYLPH_WEB_BACKEND: `http://127.0.0.1:${BACKEND_PORT}` },
    },
  ],
});
