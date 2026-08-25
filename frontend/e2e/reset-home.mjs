/**
 * Wipe the suite's Sylph home. Run from the `e2e` npm script, before Playwright.
 *
 * ## Why the wipe exists
 *
 * This directory used to persist across every run, and that cost real debugging
 * time twice, for two different reasons — both recorded in `AGENTS.md`:
 *
 * 1. A spec asserting "this group grew by one" passed for months and then failed
 *    permanently once the shared group reached the sidebar's 200-row cap, because
 *    past the cap a new session *displaces* an older row instead of adding one.
 *    The diff-by-one read as a UI bug.
 * 2. Every `mkdtemp` directory a spec created became a permanent sidebar group.
 *    After enough runs the sidebar rendered 151 groups, 148 of them holding one
 *    dead single-session directory, and every page load paid for all of them. The
 *    suite went from 1.5 minutes to 35 minutes, with two or three specs failing
 *    per run on the default timeout — a *different* two or three each time, which
 *    is what made it read as flakiness rather than as a fixture problem.
 *
 * The documented remedy was to run `rm -rf frontend/.e2e-home` by hand before
 * trusting a timing result. That is the right action in the wrong place: it relies
 * on a human remembering, the failure it prevents is silent, and symptom 2 is
 * indistinguishable from flakiness at the moment you most need to trust the suite.
 *
 * ## Why it runs from the npm script and not from Playwright
 *
 * Two Playwright hooks look right and are both wrong, measured on 1.60.0:
 *
 * - **`globalSetup` runs too late.** `createGlobalSetupTasks`
 *   (`playwright/lib/runner/index.js`) orders plugin setup *before* every
 *   `globalSetup`, and `webServer` is a plugin. So the hook fires with both
 *   servers already up, and the wipe deletes `journal.db` out from under a backend
 *   that has opened it: every query then fails `unable to open database file` and
 *   the suite answers 500s.
 * - **Module scope in `playwright.config.ts` runs too often.** Each worker process
 *   re-imports the config, so a side effect there fires again *during* the run —
 *   same deletion under a live backend, but now eight seconds in, which reads as a
 *   mid-run database failure rather than as a fixture wipe.
 *
 * A separate process invoked by the script has the one property both lack: it runs
 * exactly once, to completion, before Playwright is asked to start anything.
 *
 * ## Why the wipe is safe
 *
 * The directory is gitignored, the backend recreates it and seeds `data/`,
 * `agents/`, `scratch/`, and `config.toml` on start exactly as a first run does,
 * and nothing in it is an input to any spec. A spec that *needs* prior state is
 * asserting on shared accumulation, which is the bug this prevents rather than a
 * use case it breaks.
 *
 * Set `SYLPH_E2E_KEEP_HOME=1` to inspect the journal after a run. That is a
 * debugging aid, not a mode to leave on: with it set, both failure modes are back.
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const home = path.resolve(path.join(here, "..", ".e2e-home"));

if (process.env.SYLPH_E2E_KEEP_HOME === "1") {
  console.log(`[e2e] keeping ${home} (SYLPH_E2E_KEEP_HOME=1)`);
  process.exit(0);
}

// Refuse to delete anything that is not the directory this file names. A
// recursive delete driven by a computed path deserves the check even when the
// path is a constant, because the constant is one edit away from not being one.
if (path.basename(home) !== ".e2e-home") {
  console.error(`[e2e] refusing to wipe an unexpected path: ${home}`);
  process.exit(1);
}

if (fs.existsSync(home)) {
  const before = Date.now();
  fs.rmSync(home, { recursive: true, force: true });
  console.log(`[e2e] wiped ${home} in ${Date.now() - before}ms`);
}
