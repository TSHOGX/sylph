import { expect, test } from "./fixtures/test";
import { createSession } from "./fixtures/session";

/**
 * The critical path: create a session, send a turn, watch it stream, and see it
 * settle. This is the flow the README promises a new user, so a break here means
 * a broken first run.
 *
 * Auth needs no setup: the app bootstraps its own cookie from
 * `GET /api/v1/auth/token` on mount.
 *
 * Selectors are scoped to `#main` where the page and the shell would otherwise
 * both match. The left sidebar lists sessions as `listitem`s too, so an unscoped
 * role query finds sidebar chrome rather than page content.
 */
test("a new session accepts a turn and streams the reply", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();

  // `Mod+N` opens a directory picker, not a form. This is the one spec that
  // drives that path through the UI — the rest create their precondition through
  // the API — so it pins both that the shortcut dispatches through `runAction`
  // and that choosing a folder is the whole interaction.
  await page.keyboard.press("ControlOrMeta+Shift+o");
  const picker = page.getByRole("dialog");
  await expect(picker).toBeVisible();

  // It opens on known workspaces, ranked by recency — a new session almost always
  // belongs in a directory this machine has already worked in. Asserted by the
  // filter's own name rather than by a row, because whether any workspace exists
  // depends on shared `.e2e-data` this spec must not count.
  await expect(picker.getByRole("combobox", { name: /filter workspaces/i })).toBeVisible();

  // Then the filesystem, for a directory that has genuinely never been used.
  // `SYLPH_WEB_BROWSE_ROOTS` is `/tmp`, so browsing opens there and the confirm
  // button needs no navigation. Choosing the directory is the only input a
  // session takes: no agent select, no title, no path typed from memory.
  await picker.getByRole("button", { name: "Browse folders..." }).click();
  await expect(picker.getByRole("listbox", { name: "Folders" })).toBeVisible();
  await picker.getByRole("button", { name: "Start session here" }).click();

  // Creation navigates to the session, so the composer is the signal it worked.
  const composer = page.getByLabel("Message", { exact: true });
  await expect(composer).toBeVisible();

  await composer.fill("Hello from Playwright");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // The fixture captures an assistant delta followed by the completed item.
  await expect(page.getByText("Hello from the E2E runner.", { exact: true })).toBeVisible();

  await expect(page).toHaveURL(/\/agents\//);
  await expect(page.locator("[data-trace-address]").filter({ hasText: "Hello from Playwright" }).first()).toBeVisible();

});

/**
 * A created session is offered for resumption on the start screen.
 *
 * This replaced an assertion against the "All sessions" list. The screen no longer
 * lists everything — it lists the few newest — so what is worth pinning is
 * narrower: the session just created is reachable from `/` in one click. Origin is
 * no longer asserted here because the row badges only `native`, which is the
 * exception that changes identity and lifecycle policy; a `sylph` badge on every
 * row of a five-row resume list was noise, and the sidebar carries the marker for
 * both cases.
 */
test("a created session is offered for resumption on the start screen", async ({ page }) => {
  await createSession(page);

  await page.goto("/");

  const recents = page.locator("#main").getByRole("listitem");
  await expect(recents.first()).toContainText(/codex|tmp/);

  // The row links to the conversation, which is the only thing this list is for.
  await recents.first().getByRole("link").click();
  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
});

test("reloading a conversation preserves its retained messages", async ({ page }) => {
  await createSession(page);

  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("echo Persist this");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Persist this", { exact: true })).toBeVisible();

  // A reload drops the observation connection and client caches. The follow-up
  // must remain readable from retained evidence, independently of the setup reply.
  await page.reload();

  await expect(page.getByText("Persist this", { exact: true })).toBeVisible();
  // The fixture's promotion turn intentionally owns the session title.  Reloading
  // must preserve the later user message as a turn, not replace the title with a
  // subsequent prompt.
  await expect(page.locator("[data-trace-address]").filter({ hasText: "echo Persist this" }).first()).toBeVisible();
});

/** The session a run creates must show up under its workspace in the sidebar. */
test("the sidebar groups a new session under its workspace", async ({ page }) => {
  await createSession(page);

  const sidebar = page.getByRole("complementary", { name: "Workspaces" });
  await expect(sidebar).toBeVisible();

  // Grouping is by workdir, so `/tmp` becomes a workspace named `tmp`.
  //
  // `aria-expanded` addresses the group's own header rather than the overflow menu
  // button beside it, which shares its name — that button exists because a group's
  // actions were otherwise right-click-only, and a phone has no right-click.
  const group = sidebar.locator("button[aria-expanded]").filter({ hasText: "tmp" });
  await expect(group.first()).toBeVisible();

  // The open session is marked as the current page for assistive technology.
  await expect(sidebar.locator('a[aria-current="page"]')).toBeVisible();
});
