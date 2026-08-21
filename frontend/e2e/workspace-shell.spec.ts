import { expect, test, type Page } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createSession } from "./fixtures/session";

/**
 * The Phase 6 shell: three panes, keyboard-driven overlays, and the Artifacts
 * file browser.
 *
 * These paths are keyboard- and focus-dependent, which is exactly what a static
 * audit cannot check. Whether `Cmd+K` reaches a document listener while a
 * composer has focus, and whether a modal returns focus to its opener, are both
 * real-browser behaviours.
 *
 * `ControlOrMeta` is Playwright's platform-resolving modifier, so one spec covers
 * the Cmd and Ctrl bindings without branching on the runner's OS.
 */

test("both sidebars toggle by keyboard and the state persists across navigation", async ({
  page,
}) => {
  await createSession(page);

  const left = page.getByRole("complementary", { name: "Workspaces" });
  const right = page.getByRole("complementary", { name: "Artifacts" });

  await expect(left).toBeVisible();
  // The Artifacts pane starts hidden: it is only useful once a file is chosen.
  await expect(right).toBeHidden();

  await page.keyboard.press("ControlOrMeta+Backslash");
  await expect(right).toBeVisible();

  await page.keyboard.press("ControlOrMeta+b");
  await expect(left).toBeHidden();

  // Persisted in localStorage, so a reload keeps the layout the user chose.
  await page.reload();
  await expect(page.getByRole("complementary", { name: "Workspaces" })).toBeHidden();
  await expect(page.getByRole("complementary", { name: "Artifacts" })).toBeVisible();

  await page.keyboard.press("ControlOrMeta+b");
  await expect(page.getByRole("complementary", { name: "Workspaces" })).toBeVisible();
});

test("the command palette opens, filters, and runs an action", async ({ page }) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+p");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  const input = dialog.getByRole("combobox");
  await expect(input).toBeFocused();

  // Fuzzy matching on a subsequence: "artif" must reach the artifacts toggle.
  await input.fill("artif");
  const option = dialog.getByRole("option").first();
  await expect(option).toContainText(/artifacts/i);

  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  // The palette dispatches through the same path as the shortcut, so running the
  // command must have the same effect as pressing the key.
  await expect(page.getByRole("complementary", { name: "Artifacts" })).toBeVisible();
});

test("global search finds a session and opens it", async ({ page }) => {
  await createSession(page);
  const sessionUrl = page.url();

  // A second session, so search has to choose between them rather than matching
  // whatever happens to be first.
  await createSession(page, { title: "Findable needle session" });

  await page.goto(sessionUrl);
  // The shortcut listener is attached by an effect, so it does not exist until
  // hydration finishes. Waiting for a client-rendered control first avoids
  // pressing the key into a page that is still static markup — a real race on a
  // cold load, not a test artifact.
  await expect(page.getByLabel("Message")).toBeVisible();
  await page.keyboard.press("ControlOrMeta+k");

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("combobox").fill("needle");

  const first = dialog.getByRole("option").first();
  await expect(first).toContainText(/Findable needle session/);
  await page.keyboard.press("Enter");

  await expect(dialog).toBeHidden();
  await expect(page.getByRole("heading", { level: 1 })).toContainText(
    "Findable needle session",
  );
});

test("Escape closes an overlay and returns focus to its opener", async ({ page }) => {
  await createSession(page);

  const opener = page.getByRole("button", { name: /Open settings/ });
  await opener.click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(":focus")).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // The regression this guards: without focus restoration a keyboard user is
  // dropped at the top of the document and must tab back to where they were.
  await expect(opener).toBeFocused();
});

test("settings persist a preference and the timeline honours it", async ({ page }) => {
  await createSession(page);

  await page.getByLabel("Message").fill("Say something");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  const thinking = page.getByText("Considering the request.");
  await expect(thinking).toBeHidden();

  await page.keyboard.press("ControlOrMeta+Comma");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByLabel("Expand thinking blocks").check();
  await page.keyboard.press("Escape");

  // The preference takes effect without a reload, and survives one.
  await expect(thinking).toBeVisible();
  await page.reload();
  await expect(page.getByText("Considering the request.")).toBeVisible();
});

test("the Artifacts pane browses the workspace and previews a file", async ({ page }) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+Backslash");
  const pane = page.getByRole("complementary", { name: "Artifacts" });
  await expect(pane).toBeVisible();

  // The session runs in `/tmp`, which the backend serves as the tree root. The
  // e2e backend writes its own data directory elsewhere, so what is listed here
  // is whatever `/tmp` holds — the assertion is on the tree existing and being
  // navigable, not on any particular file being present.
  const tree = pane.getByRole("tree");
  await expect(tree).toBeVisible();

  // Confirm the root path shown is the session's workdir, canonicalized. On
  // macOS `/tmp` is a symlink to `/private/tmp`, so both spellings are accepted:
  // the backend reports the canonical path, which is the correct behaviour.
  await expect(pane.getByText(/^\/(private\/)?tmp$/)).toBeVisible();
});

test("expand-all and collapse-all drive every disclosure in the timeline", async ({
  page,
}) => {
  await createSession(page);

  await page.getByLabel("Message").fill("Think about this");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // Wait for the turn to settle before asserting the collapsed default. Thinking
  // is deliberately held open *while* it streams, and the reply block now appears
  // during the turn rather than after it, so the reply alone no longer implies the
  // turn is over. The composer re-enabling is what actually marks that.
  await expect(page.getByLabel("Message")).toBeEnabled();

  const thinking = page.getByText("Considering the request.");
  await expect(thinking).toBeHidden();

  // Shift+E is a bare-key binding, so it must not fire while the composer has
  // focus. Clicking the transcript first is what a user would do, and is also
  // what proves the typing-target guard works: pressing it in the composer below
  // must not expand anything.
  await page.getByLabel("Message").focus();
  await page.keyboard.press("Shift+E");
  await expect(thinking).toBeHidden();

  await page.getByRole("button", { name: "Expand all" }).click();
  await expect(thinking).toBeVisible();

  await page.getByRole("button", { name: "Collapse all" }).click();
  await expect(thinking).toBeHidden();
});

/**
 * Session rows inside one sidebar group.
 *
 * Counted rather than compared against a literal: the E2E data directory is not
 * wiped between runs, so absolute counts depend on how many times the suite has
 * been run before. Each group labels its own list, which is what makes one
 * group's rows addressable without depending on sidebar ordering.
 */
function groupRows(page: Page, name: string) {
  return page.getByRole("list", { name: `Sessions in ${name}` }).getByRole("link");
}

test("Quick Sessions creates a session from its menu with no dialog", async ({ page }) => {
  // The regression: "New session here" used to route to a create form and ask
  // again for a working directory the right-clicked group had already named.
  await createSession(page);

  const rows = groupRows(page, "Quick Sessions");
  const before = await rows.count();

  // The default agent is unset here, so creation falls back to the first
  // discovered runner — the E2E fixture, which is the only one installed.
  await page.getByRole("button", { name: /Quick Sessions/ }).click({ button: "right" });
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  await menu.getByRole("menuitem", { name: "New session here", exact: true }).click();

  // Straight to a usable conversation: no form, and never the create route.
  await expect(page.getByLabel("Message")).toBeVisible();
  await expect(page).toHaveURL(/\/sessions\/(?!new)[0-9a-f-]{36}/);
  await expect(page.getByLabel("Working directory")).toHaveCount(0);

  // Created in the fixed Quick Start directory, so it joins that group rather
  // than spawning a workspace of its own.
  await expect(rows).toHaveCount(before + 1);
});

test("a workspace group creates in its own directory, with one menu item and no form", async ({
  page,
}) => {
  // A directory this run owns, rather than `/tmp`.
  //
  // The sidebar caps its listing at 200 sessions, and the e2e data directory is
  // not wiped between runs, so a shared `/tmp` group creeps toward that cap until
  // `before + 1` stops being reachable and this spec fails for reasons having
  // nothing to do with what it tests. A fresh directory makes the group start
  // empty, so the count is exact and the assertion means what it says.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sylph-e2e-group-"));
  const name = path.basename(dir);
  await createSession(page, { workdir: dir });

  const group = page.getByRole("button", { name, exact: false }).first();
  const rows = groupRows(page, name);
  await expect(rows).toHaveCount(1);

  await group.click({ button: "right" });
  await page
    .getByRole("menu")
    .getByRole("menuitem", { name: "New session here", exact: true })
    .click();

  await expect(page.getByLabel("Message")).toBeVisible();
  // The directory was inherited from the group, not re-asked.
  await expect(rows).toHaveCount(2);

  // Exactly one create item in the menu. There were two — one creating directly,
  // one opening a prefilled form — which put the same words on two entries that
  // behaved differently, and made the slower one look like the careful choice.
  // The form is gone, so this asserts the menu did not keep a second door to it.
  await page.getByRole("button", { name, exact: false }).first().click({ button: "right" });
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem", { name: /^New session here/ })).toHaveCount(1);
});

/**
 * Archive browsing with no archive.
 *
 * The e2e backend points `TRACEDB_BIN` at a path that does not exist, so these
 * cover the degradation path: on a machine without TraceDB the sidebar must still
 * list live sessions, and the history browser must say the archive is unavailable
 * rather than render an error or hang. That is the state every user without a
 * TraceDB build is in, which makes it the more important of the two paths to pin.
 */
test("the history browser opens by keyboard and reports an unavailable archive", async ({
  page,
}) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+Shift+k");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  // Scoped to the open session's workspace, not a global list.
  await expect(dialog.getByRole("combobox", { name: /filter this workspace/i })).toBeFocused();
  await expect(dialog.getByText(/archive is unavailable/i)).toBeVisible();

  // Escape closes it, like every other overlay.
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

/**
 * Switching which workspace the history browser reads.
 *
 * This replaced a `<select>`, which on a real archive is 200+ options sharing a
 * `~/Workspace/` prefix and reachable only by first letter. What matters here is
 * that the switcher filters on any part of the path and hands focus to its own
 * input — a mode swap inside one modal, so a second focus trap is never mounted.
 */
test("the history browser switches workspace through a filterable picker", async ({
  page,
}) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+Shift+k");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  await dialog.getByRole("button", { name: /^Workspace:/ }).click();

  // The switcher took over the same dialog, and its filter has focus — so the
  // user's first keystroke narrows workspaces rather than being swallowed.
  const filter = dialog.getByRole("combobox", { name: /filter workspaces/i });
  await expect(filter).toBeFocused();

  // Matched on the path, not just the leaf name: `/tmp` is where the session was
  // created, so that workspace exists and is findable by a path fragment.
  await filter.fill("tmp");
  const option = dialog.getByRole("option").first();
  await expect(option).toContainText("tmp");

  await option.click();
  // Back to the session list, now scoped to the chosen workspace.
  await expect(dialog.getByRole("combobox", { name: /filter this workspace/i })).toBeVisible();
  await expect(dialog.getByRole("button", { name: /^Workspace:.*tmp/ })).toBeVisible();

  // The filesystem fallback exists for a directory the archive has never seen.
  await dialog.getByRole("button", { name: /^Workspace:/ }).click();
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  await expect(dialog.getByRole("list", { name: "Folders" })).toBeVisible();

  // One trap throughout: Escape closes the whole thing, not just a nested layer.
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("the sidebar still lists live sessions when the archive is absent", async ({ page }) => {
  await createSession(page);

  const sidebar = page.getByRole("navigation", { name: "Sessions by workspace" });
  await expect(sidebar).toBeVisible();

  // The live session is listed, and no archived row is invented.
  await expect(sidebar.getByRole("link").first()).toBeVisible();
  await expect(sidebar.locator('a[href^="/archive/"]')).toHaveCount(0);
});

test("an archived session missing from the archive says so, with no composer", async ({
  page,
}) => {
  // Also covers the catch-all route decoding a colon-bearing trace id: a
  // double-encoded id would look up a literal `claude%3A...` and report the wrong
  // thing.
  await page.goto(`/archive/${encodeURIComponent("claude:does-not-exist")}`);

  // A heading, not `role=alert`: Next mounts its own empty route announcer with
  // that role, so matching on the role alone is ambiguous.
  await expect(page.getByRole("heading", { level: 1 })).toContainText(
    /archive|could not load/i,
  );
  // Asserted on the controls themselves, not on `getByLabel("Message")`: that
  // matches accessible names by substring, so every `SendMessage tool call`
  // section in a transcript satisfies it — 13 of them in one real session. The
  // assertion would then pass whether or not a composer was present.
  await expect(page.locator("textarea")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Send$/ })).toHaveCount(0);
});
test("binding a workspace from the empty sidebar area needs no session first", async ({
  page,
}) => {
  await createSession(page);

  const sidebar = page.getByRole("complementary", { name: "Workspaces" });

  // Reachable by right-click on pane chrome. Aimed at the footer rather than the
  // list area: group and session rows deliberately stop propagation so their own
  // menus win, and once sessions fill the scroll container they cover all of it.
  await sidebar
    .getByRole("button", { name: "New Workspace..." })
    .click({ button: "right" });
  const menu = page.getByRole("menu", { name: "Workspace list actions" });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "New Workspace..." })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();

  // And as a visible button, since a right-click gesture alone would leave the
  // only path to binding a fresh directory undiscoverable.
  await sidebar.getByRole("button", { name: "New Workspace..." }).click();

  // The picker enumerates the *backend's* filesystem, which a browser cannot do
  // for itself.
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("list", { name: "Folders" })).toBeVisible();

  await dialog.getByRole("button", { name: "Bind workspace" }).click();

  // `/tmp` is a configured browse root and the picker's landing directory, so
  // binding it makes a group with no sessions of its own — which is the point:
  // a workspace can exist before any session does.
  await expect(dialog).toBeHidden();
  await expect(sidebar.getByRole("button", { name: /tmp/ }).first()).toBeVisible();
});

// -------------------------------------------------------- Phase 7: composer
//
// The composer now exposes the runner configuration a turn will use. Both
// controls are generated from `--describe`, so these assert against whatever the
// E2E fixture declares rather than hardcoding a model list.

test("the composer exposes the agent and its declared flags, and locks the agent after the first turn", async ({
  page,
}) => {
  await createSession(page);

  // Before any turn the agent is changeable: nothing has been produced by it yet.
  const agent = page.getByLabel("Agent", { exact: true });
  await expect(agent).toBeVisible();
  await expect(agent).toHaveValue("e2e");

  const composer = page.getByLabel("Message");
  await composer.fill("First turn");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // After a turn it is a plain label, not a disabled control: switching runner
  // would hand an existing transcript to a different identity policy.
  await expect(page.getByLabel("Agent", { exact: true })).toHaveCount(0);
  await expect(page.getByTitle(/agent is fixed once a session has history/)).toBeVisible();
});

test("attachments stage inside the composer rather than in a separate dropzone", async ({
  page,
}) => {
  await createSession(page);

  // The old stacked "Attach files or drop them here" strip is gone; the paperclip
  // inside the composer replaces it.
  await expect(page.getByText("Attach files or drop them here.")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Attach files" })).toBeVisible();

  await page.setInputFiles('input[type="file"]', {
    name: "note.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello"),
  });

  // Staged as a chip on the composer's own surface.
  const staged = page.getByRole("list", { name: "Staged attachments" });
  await expect(staged.getByText("note.txt")).toBeVisible();

  // Offered only once something is staged, and writes into the workspace subtree
  // rather than the per-turn staging directory.
  await expect(page.getByRole("button", { name: /Save to workspace/ })).toBeVisible();

  await staged.getByRole("button", { name: "Remove note.txt" }).click();
  await expect(staged).toHaveCount(0);
});
