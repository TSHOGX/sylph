import { expect, type Page } from "@playwright/test";

/**
 * Creating the session a spec needs as a precondition.
 *
 * Most specs here assert something *about* a session — the sidebar grouping, the
 * timeline, an overlay opened over it — and the session itself is setup, not the
 * subject. Those go through the API: it is one request instead of a picker
 * round trip, and it stops a change to the create UI from failing a dozen specs
 * that were never testing it.
 *
 * The create *interaction* is covered by `workspace-shell.spec.ts`, which drives
 * the directory picker and the sidebar menus for real. That split is deliberate:
 * one spec owns the path, the rest state their precondition.
 */

/** The mock runner installed by `playwright.config.ts`. The only agent present. */
const AGENT = "e2e";

/**
 * Open a page and wait until keyboard shortcuts actually work.
 *
 * A shortcut pressed straight after `goto` races hydration. The document Next
 * serves for `/` contains exactly two pieces of text — "Skip to main content" and
 * "Connecting to backend..." — so every control on the start screen is
 * client-rendered, and the shell's `keydown` listener is attached by an effect that
 * has not run yet. The key lands on static markup and is discarded, then the spec
 * waits 10s for a dialog that was never asked to open.
 *
 * This is a real race on a cold load, not a test artifact, and it is invisible on a
 * fast machine: it surfaced in 1 of 4 identical suite runs, failing two specs that
 * had passed hundreds of times.
 *
 * Gating on `New session` is what makes this sound rather than a sleep. That button
 * does not exist until React has hydrated *and* `AuthGate` has resolved, and the
 * shell owning the listener is its ancestor — so once the button is on screen, the
 * effect that attaches the listener has necessarily run.
 *
 * Use this instead of `page.goto()` in any spec whose next action is a shortcut.
 * Specs that click their way in do not need it, and specs that go through
 * `createSession` already wait for the composer.
 */
export async function gotoReady(page: Page, url = "/") {
  await page.goto(url);
  await expect(page.getByRole("button", { name: /New session/ })).toBeVisible();
}

/**
 * Create a session through the API and open it.
 *
 * Requests go through the Next proxy rather than straight to the backend: the
 * cookie is `SameSite=Strict`, so a browser would not send it cross-origin to
 * another port, and using the same origin as the app keeps one cookie jar.
 *
 * The token is bootstrapped here rather than relying on a prior navigation.
 * `page.request` shares the browser context's jar, so `AuthGate` mounting *would*
 * eventually set it — but that is a render, and a request issued before it lands
 * fails with a 401 that reads like a broken backend. One explicit GET removes the
 * race, and the endpoint is idempotent, so calling it per fixture is free.
 */
export async function createSession(
  page: Page,
  { workdir = "/tmp", title }: { workdir?: string; title?: string } = {},
): Promise<string> {
  const bootstrap = await page.request.get("/api/v1/auth/token");
  expect(bootstrap.ok(), `auth bootstrap failed: ${bootstrap.status()}`).toBe(true);

  // `title` is set here but has no control in the UI. It remains on the model and
  // the API — an imported native session carries the originating agent's title —
  // and search matches it, which is what a spec needs to make one session
  // findable among several. The create picker deliberately does not ask for it:
  // a session is labelled by its opening message, which the user is about to
  // type anyway.
  const response = await page.request.post("/api/v1/sessions", {
    data: { agent: AGENT, workdir, ...(title ? { title } : {}) },
  });
  expect(
    response.ok(),
    `session create failed: ${response.status()} ${await response.text()}`,
  ).toBe(true);

  const { session } = (await response.json()) as { session: { id: string } };
  await page.goto(`/sessions/${session.id}`);
  await expect(page.getByLabel("Message")).toBeVisible();
  return session.id;
}
