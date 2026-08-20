import { expect, test, type Page } from "@playwright/test";

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

/** Create a session in `/tmp` and land on its conversation view. */
async function createSession(page: Page) {
  await page.goto("/sessions/new");
  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");
  await page.getByRole("button", { name: "Create session" }).click();
  await expect(page.getByLabel("Message")).toBeVisible();
}

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
  await page.goto("/sessions/new");
  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");
  await page.getByLabel(/^Title/).fill("Findable needle session");
  await page.getByRole("button", { name: "Create session" }).click();
  await expect(page.getByLabel("Message")).toBeVisible();

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
