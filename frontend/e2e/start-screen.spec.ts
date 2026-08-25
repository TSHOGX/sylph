import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { expect, test } from "@playwright/test";
import { createSession, gotoReady } from "./fixtures/session";

/**
 * `/` — the start screen.
 *
 * What is worth asserting here is not the layout but the three properties that
 * made it replace an "All sessions" list with filters:
 *
 * - it is usable before any request settles, because a start screen that waits on
 *   the network is a blank pane on every launch;
 * - it distinguishes "no sessions yet" from "not loaded yet", which the old page
 *   conflated — both are an empty array, and guessing wrong flashes onboarding at
 *   a user who has hundreds of sessions;
 * - it teaches the shortcuts and the create paths, since those are otherwise
 *   discoverable only through the settings panel, which is itself a shortcut.
 *
 * The first-run state is produced by intercepting the listing rather than by
 * emptying the journal. Specs share one backend and run serially, so by the time
 * this file runs other sessions exist; interception is the only way to assert the
 * empty state deterministically, and it is the *client's* branch being tested.
 */

test("the start screen is usable before the sessions listing settles", async ({ page }) => {
  // Hold the listing open. Everything asserted below must be on screen anyway:
  // this is the difference between a start screen and a loading screen.
  await page.route("**/api/v1/sessions?*", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 4_000));
    await route.continue();
  });

  await page.goto("/");

  await expect(page.getByRole("heading", { level: 1, name: "Sylph" })).toBeVisible();
  await expect(page.getByRole("button", { name: /New session/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Quick session/ })).toBeVisible();
  // The keys are static content, so they are readable while the listing is still
  // in flight rather than after it.
  await expect(page.getByRole("heading", { name: "Keys" })).toBeVisible();

  // And the pending listing must not be reported as an empty app.
  await expect(page.getByRole("heading", { name: "Getting started" })).toBeHidden();
});

test("an app with no sessions gets the first-run guide, not an empty list", async ({
  page,
}) => {
  await page.route("**/api/v1/sessions?*", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );

  await page.goto("/");

  const guide = page.getByRole("region", { name: "Getting started" });
  await expect(guide).toBeVisible();
  // The one fact the rest of the UI assumes and never states. A user who does not
  // know it picks the wrong directory, and the agent then works in the wrong tree.
  await expect(guide).toContainText(/bound to one directory/i);
  // Existing agent history is reachable without creating anything, which is the
  // other thing a first-run user cannot guess.
  await expect(guide.getByRole("button", { name: "Browse past work" })).toBeVisible();
});

test("the first-run guide gives way to recents once a session exists", async ({ page }) => {
  await createSession(page);
  await page.goto("/");

  await expect(page.getByRole("region", { name: "Pick up where you left off" })).toBeVisible();
  // Onboarding is not sticky: it is gone for good after the first create, so a
  // returning user pays nothing for it.
  await expect(page.getByRole("heading", { name: "Getting started" })).toBeHidden();
});

/**
 * Many sessions in one directory are one row, not many.
 *
 * This is the property that makes a five-row list worth having. Before grouping, a
 * directory worked in all week filled every row with entries that differed only by
 * a timestamp — and since most sessions are untitled, were otherwise identical —
 * pushing every other place the user works off the list entirely.
 */
test("a directory with several sessions is one row that opens the newest", async ({
  page,
}) => {
  // A directory of this spec's own, not the shared `/tmp`. The journal is never
  // cleared between runs, so every other spec's `/tmp` session lands in the same
  // group — and "the newest here" then means the newest of *those*, not of the three
  // created below. That made this assert pass on a clean data directory and fail on
  // an accumulated one, which reads as flakiness rather than as shared state.
  const workdir = mkdtempSync(`${tmpdir()}/sylph-recents-`);

  await createSession(page, { workdir });
  await createSession(page, { workdir });
  const newest = await createSession(page, { workdir, title: "Newest here" });

  await page.goto("/");
  const recents = page.getByRole("region", { name: "Pick up where you left off" });

  // One row for the directory, whatever the session count, and it states the count.
  const tmpRow = recents.getByRole("listitem").filter({ hasText: workdir });
  await expect(tmpRow).toHaveCount(1);
  await expect(tmpRow).toContainText("3 sessions");

  // And it opens the newest session, which is what "resume" means.
  await tmpRow.getByRole("link").click();
  await expect(page).toHaveURL(new RegExp(`/sessions/${newest}$`));
});

test("the start screen's create button runs the same path as its shortcut", async ({
  page,
}) => {
  await page.goto("/");

  // A button, not a link to a create route: the screen advertises the shortcut and
  // the click must land in the same place, or the hint it renders is a lie.
  await page.getByRole("button", { name: /New session/ }).click();
  const picker = page.getByRole("dialog");
  await expect(picker).toBeVisible();
  await expect(picker.getByRole("combobox", { name: /filter workspaces/i })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(picker).toBeHidden();

  await page.keyboard.press("ControlOrMeta+Shift+o");
  await expect(page.getByRole("dialog")).toBeVisible();
});

test("the keys shown are the bindings in effect, not hard-coded captions", async ({
  page,
}) => {
  // `gotoReady` waits for a client-rendered control, because the shortcut below is
  // dropped if it arrives before the shell's keydown effect runs — which is exactly
  // how this spec failed in 1 of 4 identical suite runs.
  await gotoReady(page);

  // Rebind through the settings panel, then check the start screen reports the new
  // combo. A hard-coded `⌘N` would satisfy every assertion above and still be
  // wrong for any user who rebound anything — and wrong on Linux for everyone.
  await page.keyboard.press("ControlOrMeta+Comma");
  const settings = page.getByRole("dialog");
  await expect(settings).toBeVisible();

  await settings.getByRole("button", { name: "Shortcuts" }).click();
  await settings.getByRole("button", { name: /^New session shortcut/ }).click();
  await page.keyboard.press("ControlOrMeta+Shift+j");
  await page.keyboard.press("Escape");
  await expect(settings).toBeHidden();

  const keys = page.getByRole("region", { name: "Keys" });
  await expect(keys.getByText(/⇧J|Shift\+J/)).toBeVisible();
  // And the create button's own hint moves with it, since both read the same source.
  await expect(page.getByRole("button", { name: /New session/ })).toContainText(/⇧J|Shift\+J/);
});
