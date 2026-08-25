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
 *
 * Both are overridable because a worktree is a second checkout of this same
 * config: two suites running side by side would otherwise bind the same two
 * ports, and the loser reports a server that failed to start rather than a port
 * that was taken. Set `SYLPH_E2E_PORT_OFFSET` per worktree — one number moves
 * both ports together, so they cannot be offset inconsistently.
 */
const PORT_OFFSET = Number(process.env.SYLPH_E2E_PORT_OFFSET ?? 0);
const BACKEND_PORT = 4318 + PORT_OFFSET;
const FRONTEND_PORT = 3101 + PORT_OFFSET;
/**
 * The suite's own Sylph home.
 *
 * One directory rather than a variable per path, which is the point of the single
 * root: the backend seeds `data/`, `agents/`, `scratch/`, and a `config.toml`
 * inside it exactly as a first run does, so E2E exercises the real first-run path
 * instead of a hand-assembled approximation of it.
 *
 * Named `.e2e-home` rather than `.e2e-data` because it now holds more than data,
 * and because the rename makes the stale `.e2e-data` from an older checkout
 * visibly unused rather than silently half-read.
 */
const sylphHome = path.join(__dirname, ".e2e-home");

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
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
      // The mobile project owns these, and running them at 1280px would assert the
      // desktop layout against mobile expectations.
      testIgnore: /mobile\.spec\.ts/,
    },
    /**
     * A phone-sized project, for the specs that cannot be written any other way.
     *
     * The three reported mobile failures — a composer wider than the viewport,
     * sidebars that crush the conversation to ~150px, and a scroll that drags the
     * whole shell — are all *layout* bugs. Every one of them passed the desktop suite
     * at 1280px, because at that width the same code is merely a little cramped. A
     * viewport is therefore not an optional variant here; it is the only thing that
     * can observe the defect.
     *
     * Pixel 7 rather than an iPhone profile: Playwright's WebKit browser is not
     * installed in this checkout (only Chromium is), and an iPhone profile silently
     * runs in Chromium anyway — which would claim iOS coverage it does not have.
     * Pixel 7 is a Chromium device, so what it emulates is what actually runs:
     * `pointer: coarse`, a 412px viewport, and a device pixel ratio above 1.
     *
     * What this therefore does *not* cover is iOS Safari specifically: `100dvh`
     * behaviour under a collapsing toolbar, and Safari's zoom-on-small-input. Those
     * are the two mechanisms behind the scroll bug, so they still need a real device
     * — which is why the fix is asserted here through its observable consequences
     * (nothing overflows, the document does not scroll) rather than by simulating a
     * toolbar this browser does not have.
     */
    {
      name: "mobile",
      use: { ...devices["Pixel 7"] },
      testMatch: /mobile\.spec\.ts/,
    },
  ],
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
        SYLPH_HOME: sylphHome,
        SYLPH_PORT: String(BACKEND_PORT),
        // Pinned to the fixtures tree, and pinned for *both* roots. Left unset,
        // bundled-adapter resolution finds the repo's real `adapters/`, so the
        // suite would discover `claude` and `pi` and run the developer's actual
        // agent CLIs — spawning real processes, and making specs pass or fail on
        // which agents happen to be installed. Naming it keeps the fixture the
        // only agent that exists.
        SYLPH_AGENTS_DIR: path.join(__dirname, "e2e", "fixtures", "runners"),
        SYLPH_BUNDLED_AGENTS_DIR: path.join(__dirname, "e2e", "fixtures", "runners"),
        // Point at a path that does not exist so archive-dependent screens
        // exercise their unavailable state instead of the developer's archive.
        SYLPH_TRACEDB_BIN: path.join(sylphHome, "absent-trace-db"),
        // Browse roots always include `$HOME`, but the specs create sessions in
        // `/tmp`. Naming it keeps the directory picker assertable without
        // depending on what happens to live in the developer's home directory.
        SYLPH_BROWSE_ROOTS: "/tmp",
        // No identity, so the fixture adapter is not handed a `SOUL.md` the specs
        // never assert on. The protocol reserves the empty string for this.
        SYLPH_SOUL: "",
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
