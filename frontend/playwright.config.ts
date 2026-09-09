import { defineConfig, devices } from "@playwright/test";
import os from "node:os";
import path from "node:path";

/**
 * End-to-end configuration.
 *
 * The server and a separately compiled envagent are started by Playwright so a run needs
 * no manual setup. The envagent is paired over the loopback wire before tests begin; this
 * keeps E2E on the Milestone 1 product path rather than reaching a linked adapter in the
 * server process. Both processes use throwaway roots, so E2E never touches the developer's
 * real journal, real archive, or real agents.
 *
 * Ports come from the allocation in `docs/configuration.md` § Ports: a server
 * instance owns an HTTP port and the port above it, which is where its envagent
 * ingress binds. Instances are spaced ten apart, so the suite's 4327 sits clear
 * of a development server on 4317 *and* of the 4318 that server's envagent
 * ingress derives — the collision the previous 4318 here produced, which
 * presented as a suite that failed to start whenever a dev envagent was running.
 *
 * Overridable because a worktree is a second checkout of this same config: two
 * suites side by side would otherwise bind the same ports, and the loser reports
 * a server that failed to start rather than a port that was taken. Set
 * `SYLPH_E2E_PORT_OFFSET` to a multiple of ten per worktree — one number moves
 * both ports together, so they cannot be offset inconsistently.
 */
const PORT_OFFSET = Number(process.env.SYLPH_E2E_PORT_OFFSET ?? 0);
const BACKEND_PORT = 4327 + PORT_OFFSET;
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
      // The wrapper starts the backend, obtains a one-shot pairing code, runs the
      // separately compiled fixture envagent, and leaves both children attached to this
      // webServer process. The `e2e` feature is a build-time choice in each binary, so a
      // production process has no path to the fixture.
      command: "node frontend/e2e/start-backend-envagent.mjs",
      cwd: path.join(__dirname, ".."),
      url: `http://127.0.0.1:${BACKEND_PORT}/api/v1/health`,
      reuseExistingServer: false,
      // A cold `cargo run` compiles the whole backend first.
      timeout: 180_000,
      stdout: "pipe",
      stderr: "pipe",
      env: {
        SYLPH_HOME: sylphHome,
        SYLPH_PORT: String(BACKEND_PORT),
        SYLPH_ENV_PORT: String(BACKEND_PORT + 1),
        SYLPH_ENV_LISTEN: `127.0.0.1:${BACKEND_PORT + 1}`,
        // No agent-directory pins, because there are no agent directories. Adapters are
        // compiled in, so `--features e2e` above is the whole mechanism — and under that
        // feature the fixture is the *only* agent this build knows
        // (`crates/adapters/src/lib.rs`, `the_fixture_feature_offers_the_fixture_and_nothing_else`).
        //
        // Both halves of what the two deleted variables did are covered by that, and the
        // second half is the one worth naming: they kept the *developer's* agents out of
        // the run. Left unset while the backend still searched directories, bundled
        // resolution found the repo's real `adapters/`, so the suite ran whichever of
        // `claude` and `pi` happened to be installed — spawning real processes and making
        // specs pass or fail on that. Exclusivity in the build cannot be forgotten the way
        // an environment pin can.
        // Browse roots always include `$HOME`, but the specs create sessions
        // outside it. Naming these keeps the directory picker assertable without
        // depending on what happens to live in the developer's home directory.
        //
        // Both entries, and this is not redundancy. `browse_roots` bounds two
        // things (`docs/configuration.md`): what `/fs/*` may enumerate, and what a
        // request may name as a session's `workdir`. The specs use two different
        // directories for those — a literal `/tmp` as the default workdir, and
        // `os.tmpdir()` per spec via `mkdtemp`, which on macOS is
        // `/var/folders/…/T` and is *not* under `/tmp`. Naming only one leaves
        // every `mkdtemp` spec creating sessions in a directory the backend
        // refuses, which surfaces as ~a dozen unrelated-looking spec failures
        // rather than as a fixture problem.
        SYLPH_BROWSE_ROOTS: ["/tmp", os.tmpdir(), os.tmpdir().startsWith("/var/") ? `/private${os.tmpdir()}` : ""].filter(Boolean).join(":"),
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
