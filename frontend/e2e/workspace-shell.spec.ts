import { expect, test, type Page } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createSession } from "./fixtures/session";

/**
 * The shell: three panes, keyboard-driven overlays, and the Artifacts
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

/**
 * Every surface listing workspaces shares one request.
 *
 * Eight surfaces read this list and several mount together, so a per-surface query key is
 * a per-surface request. TanStack dedupes by key — but only while every declaration
 * agrees, and hand-written literals had already drifted into two `staleTime` values with
 * four of them dead, since `AppShell` keeps a shorter-lived observer mounted on every
 * route. `lib/workspaces-query.ts` is the single declaration.
 *
 * Counted over the network rather than asserted structurally: a refactor that *looks*
 * shared still fans out if one key differs by a character, and that failure is invisible
 * on screen — it costs requests, not correctness.
 */
test("the surfaces that list workspaces share one request", async ({ page }) => {
  let requests = 0;
  await page.route("**/api/v1/workspaces*", async (route) => {
    requests += 1;
    await route.fallback();
  });

  await page.goto("/");
  // The start screen and the sidebar both list workspaces, and the shell reads the same
  // response for `quickStartDir` — so first paint is where a fan-out would show up.
  await expect(page.getByRole("navigation", { name: "Sessions by workspace" })).toBeVisible();
  const afterFirstPaint = requests;
  expect(
    afterFirstPaint,
    "the shell, sidebar, and start screen must share one workspaces request",
  ).toBe(1);

  // Opening two more consumers must read the cache rather than refetch: both are well
  // inside the shared `staleTime`.
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.keyboard.press("ControlOrMeta+Shift+k");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");

  expect(requests, "an overlay inside the stale window must not refetch").toBe(
    afterFirstPaint,
  );
});

test("both sidebars toggle by keyboard and the state persists across navigation", async ({
  page,
}) => {
  await createSession(page);

  const left = page.getByRole("complementary", { name: "Workspaces" });
  const right = page.getByRole("complementary", { name: "Trace inspector" });

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
  await expect(page.getByRole("complementary", { name: "Trace inspector" })).toBeVisible();

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

  // Fuzzy matching on a subsequence: "wrk pnl" must reach the work-panel toggle.
  await input.fill("wrk pnl");
  const option = dialog.getByRole("option").first();
  await expect(option).toContainText(/work panel/i);

  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  // The palette dispatches through the same path as the shortcut, so running the
  // command must have the same effect as pressing the key.
  await expect(page.getByRole("complementary", { name: "Trace inspector" })).toBeVisible();
});

/**
 * Grouping, and the cursor that has to mean one thing in both modes.
 *
 * The palette drew its group headings itself and numbered its cursor over commands
 * while rendering over rows — two index spaces, which is the bug where the highlight,
 * `aria-activedescendant`, and Enter each point at a different row. It now builds rows
 * through `lib/select-list.ts`, where a heading is a disabled row and takes no cursor
 * position at all.
 *
 * What that is observable as: headings are present unfiltered and gone once a query
 * narrows the list, and the first Down arrow lands on the first *command* rather than
 * on the heading above it. A spec asserting only that a heading renders would pass
 * against the version that mis-numbered them.
 */
test("the palette groups commands, and the cursor skips the headings", async ({ page }) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+p");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  // A heading is a disabled option: rendered, inert, and not a cursor stop.
  const headings = dialog.getByRole("option", { disabled: true });
  await expect(headings.filter({ hasText: "Navigation" })).toHaveCount(1);

  // One Down from the opening cursor position moves within the commands. `aria-selected`
  // is the current *value* in this pattern and nothing here has one, so the cursor is
  // read off `aria-activedescendant` — the attribute that pointed at the wrong row.
  const input = dialog.getByRole("combobox");
  await input.press("ArrowDown");
  const active = await input.getAttribute("aria-activedescendant");
  expect(active).toBeTruthy();
  // An attribute selector, not `#id`: these ids come from `useId`, whose output
  // carries characters that are not valid in a CSS id selector.
  await expect(dialog.locator(`[id="${active}"]`)).toBeEnabled();

  // Filtering drops every heading, because a heading introduces rows that are no
  // longer beneath it.
  await input.fill("session");
  await expect(dialog.getByRole("option", { disabled: true })).toHaveCount(0);
  await expect(dialog.getByRole("option").first()).toBeEnabled();
});

test("global search finds a retained message and opens its addressed conversation", async ({ page }) => {
  const { installTraceFixture, message, openTrace, MAIN_AGENT } = await import("./fixtures/trace");
  await installTraceFixture(page, [message(3, "A retained hit outside the visible window")]); await openTrace(page);
  await page.keyboard.press("ControlOrMeta+k"); const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox", { name: "Search retained conversations" }).fill("needle");
  await dialog.getByRole("button", { name: "Search", exact: true }).click();
  await expect(dialog.getByText("A retained hit outside the visible window", { exact: true })).toBeVisible();
  await dialog.getByRole("link", { name: "Open this retained result", exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/agents/${MAIN_AGENT}.*anchor=`)); await expect(dialog).toHaveCount(0);
});

test("a global index hit keeps its retained excerpt and checkpoint visible", async ({ page }) => {
  const { installTraceFixture, openTrace } = await import("./fixtures/trace"); await installTraceFixture(page); await openTrace(page);
  await page.keyboard.press("ControlOrMeta+k"); const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox", { name: "Search retained conversations" }).fill("deadlock"); await dialog.getByRole("button", { name: "Search", exact: true }).click();
  await expect(dialog.getByText("A retained hit outside the visible window", { exact: true })).toBeVisible();
  await expect(dialog.getByText(/Matched Current history at retained view 1/)).toBeVisible();
});

test("Escape closes an overlay and returns focus to its opener", async ({ page }) => {
  await createSession(page);

  const opener = page.getByRole("button", { name: /Open settings/ });
  await opener.click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  // Focus is *within* the dialog, which includes the dialog element itself. A
  // `dialog.locator(":focus")` matches descendants only, and the settings panel has
  // no auto-focusing control of its own, so focus correctly lands on the container —
  // which is what makes Escape reach its handler at all.
  await expect(
    dialog.evaluate((node) => node === document.activeElement || node.contains(document.activeElement)),
  ).resolves.toBe(true);

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // The regression this guards: without focus restoration a keyboard user is
  // dropped at the top of the document and must tab back to where they were.
  await expect(opener).toBeFocused();
});

test("settings retain display density without overriding collapsed execution", async ({ page }) => {
  await createSession(page);
  await page.keyboard.press("ControlOrMeta+Comma");
  const dialog=page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("checkbox",{name:/expand/i})).toHaveCount(0);
  await dialog.getByRole("button",{name:"Comfortable",exact:true}).click();
  await page.keyboard.press("Escape");
  await page.reload();
  // Navigation finishes before the session hydrates and installs shortcuts.
  // This assertion measures persisted density, so dispatch only once interactive.
  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
  await page.keyboard.press("ControlOrMeta+Comma");
  await expect(page.getByRole("dialog").getByRole("button",{name:"Comfortable",exact:true})).toHaveAttribute("aria-pressed","true");
});

test("the Artifacts pane browses the workspace and previews a file", async ({ page }) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+Backslash");
  const pane = page.getByRole("complementary", { name: "Trace inspector" });
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

test("expand-all is absent from the session and command palette", async ({ page }) => {
  await createSession(page);
  await expect(page.getByRole("button",{name:"Expand all",exact:true})).toHaveCount(0);
  await page.keyboard.press("ControlOrMeta+Shift+p");
  await expect(page.getByRole("dialog").getByText(/Expand all/)).toHaveCount(0);
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

/**
 * A sidebar group's own header button.
 *
 * Every group now carries a second button — an overflow menu named "<name>
 * actions" — because its actions were otherwise reachable only by right-click, and
 * there is no right-click on a touch screen. A bare name match therefore resolves
 * to two elements.
 *
 * `aria-expanded` is what distinguishes them, and it is the honest discriminator
 * rather than a positional `.first()`: the header is the disclosure, so it is the
 * only one of the two that has an expanded state at all.
 */
function groupHeader(page: Page, name: string) {
  return page.locator("button[aria-expanded]").filter({ hasText: name }).first();
}

test("Quick Sessions creates a session from its menu with no dialog", async ({ page }) => {
  // The regression: "New session here" used to route to a create form and ask
  // again for a working directory the right-clicked group had already named.
  await createSession(page);

  const rows = groupRows(page, "Quick Sessions");

  // The default agent is unset here, so creation falls back to the first
  // discovered runner — the E2E fixture, which is the only one installed.
  await groupHeader(page, "Quick Sessions").click({ button: "right" });
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  await menu.getByRole("menuitem", { name: "New session here", exact: true }).click();

  // Straight to a usable conversation: no form, and never the create route.
  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
  // Creation is two-phase: this route carries an opaque draft handle until the first
  // real message. It is intentionally absent from the sidebar and history while empty.
  await expect(page).toHaveURL(/\/sessions\/draft\/(?:draft-|remote-draft-)/);
  await expect(page.getByLabel("Working directory")).toHaveCount(0);

  await page.getByLabel("Message", { exact: true }).fill("persist this draft");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  // Only promotion creates the identity triple and the durable row.
  await expect(page).toHaveURL(/\/agents\/[^/]+/);

  // Created in the fixed Quick Start directory, so it joins that group rather than
  // spawning a workspace of its own.
  //
  // Asserted as "the new session is listed under Quick Sessions", not as
  // `before + 1`. The count is not a safe assertion here and its failure mode is
  // misleading: `.e2e-data/` is never wiped, so the shared journal accumulates every
  // session from every past run — 357 at the time of writing — and the sidebar's
  // listing is capped at `LIVE_SESSIONS_LIMIT` (200). Past that cap a new session
  // displaces an older one instead of adding a row, so the count stays flat and the
  // spec reports an off-by-one that looks like a UI bug and is not. It also only
  // fails once the shared group crosses the cap, which is why it passed for months
  // and then failed permanently.
  //
  // The identity check is what the test was actually for, and it does not depend on
  // how many times the suite has run before.
  // Decoded from the URL and re-encoded for the attribute match, because the two spellings
  // differ: an id is the triple `local:e2e:<native id>`, the sidebar writes
  // `encodeURIComponent(id)` into `href`, and `page.url()` hands back the decoded form. So a
  // raw comparison looks for `local:e2e:…` in an attribute holding `local%3Ae2e%3A…` and
  // matches nothing — which reads as "the session is not listed" rather than as an encoding
  // mismatch. Invisible while an id was a bare UUID with nothing in it to escape.
  const created = decodeURIComponent(page.url().split("/").pop()!);
  // The link itself carries the href, so this is an attribute match on the row rather
  // than `filter({ has })` — which searches *descendants* and therefore matches
  // nothing here.
  await expect(
    rows.and(page.locator(`[href$="${encodeURIComponent(created)}"]`)),
  ).toHaveCount(1);
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

  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
  // The directory was inherited from the group, not re-asked. The empty draft is
  // deliberately not a sidebar row; it becomes one only after a real message.
  await expect(rows).toHaveCount(1);
  await page.getByLabel("Message", { exact: true }).fill("persist this draft");
  await page.getByRole("button", { name: "Send", exact: true }).click();
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
 * Browsing one workspace's history.
 *
 * The unavailable-archive assertion this replaces is gone with the archive: there is no
 * external store to be absent, so the degradation path it pinned does not exist. What
 * survives is the part that was always about this surface — it is scoped to a directory
 * rather than global, and it focuses its filter so the first keystroke narrows rather
 * than changing the workspace.
 */
test("the history browser opens by keyboard, scoped to the open session's workspace", async ({
  page,
}) => {
  await createSession(page);

  await page.keyboard.press("ControlOrMeta+Shift+k");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  // Scoped to the open session's workspace, not a global list — and the filter has
  // focus, because a control above it would otherwise take the first keystroke and
  // change the workspace instead of filtering.
  await expect(dialog.getByRole("combobox", { name: /filter this workspace/i })).toBeFocused();

  // Escape closes it, like every other overlay.
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

/**
 * Switching which workspace the history browser reads.
 *
 * This replaced a `<select>`, which on a real machine is 200+ options sharing a
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

  // The filesystem fallback exists for a directory nothing has ever run in.
  await dialog.getByRole("button", { name: /^Workspace:/ }).click();
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  // A `listbox`, not a `list`: the folder browser is filterable now, and a
  // filterable list has to be the combobox pattern — a `role="menu"` or a bare
  // `<ul>` cannot legally contain the text field.
  await expect(dialog.getByRole("listbox", { name: "Folders" })).toBeVisible();

  // One trap throughout: Escape closes the whole thing, not just a nested layer.
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("the sidebar lists a created session in its workspace group", async ({ page }) => {
  await createSession(page);

  const sidebar = page.getByRole("navigation", { name: "Sessions by workspace" });
  await expect(sidebar).toBeVisible();
  await expect(sidebar.getByRole("link").first()).toBeVisible();
});

// Two tests are deleted here rather than adapted, because both asserted on the
// separate archive and there is nothing left for them to assert:
//
// - "the sidebar still lists live sessions when the archive is absent" checked that no
//   `/archive/…` row was invented when the archive could not be reached. There is one
//   store, so a row is a row and the absent-archive state does not exist.
// - "an archived session missing from the archive says so" exercised the
//   `/archive/[...id]` catch-all route, including its decoding of a colon-bearing
//   trace id. That route is gone with the second id space. The decoding concern it
//   covered is *not* gone and has moved somewhere better: a session id contains `:`
//   and may contain `/`, so `api.ts`'s `segment` encodes every path interpolation, and
//   `lib/api.ts` documents why. That is now a property of every session route rather
//   than one archive route's own trick.

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

  // Known workspaces first: binding a directory that already holds history is the
  // common case — a project worked on before this UI existed — and that directory is
  // absent from the sidebar precisely because nothing has bound it yet.
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("combobox", { name: /filter workspaces/i })).toBeVisible();

  // The filesystem stays one click away, for a folder with no history at all.
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  await expect(dialog.getByRole("listbox", { name: "Folders" })).toBeVisible();

  await dialog.getByRole("button", { name: "Bind workspace" }).click();

  // `/tmp` is a configured browse root and the picker's landing directory, so
  // binding it makes a group with no sessions of its own — which is the point:
  // a workspace can exist before any session does.
  await expect(dialog).toBeHidden();
  await expect(groupHeader(page, "tmp")).toBeVisible();
});

// ----------------------------------------------------------------- composer
//
// The composer exposes the configuration a turn will use, generated from what the
// agent reports rather than hardcoded — so these assert against whatever the E2E
// fixture declares.
//
// The source changed with the v3 protocol: the control is driven by the session's
// live `config`, not by the agent descriptor's static `flags`. That is what makes it
// correct at all for a real agent — `claude` declares an empty `model.choices` in its
// descriptor because its catalogue depends on the account and is only knowable from a
// running session, so a descriptor-driven control was permanently empty.

test("a declared config option is a truncating button that opens a menu, not a <select>", async ({
  page,
}) => {
  await createSession(page);

  // The fixture reports one `model` option in the `model` category, so exactly one
  // control is expected on the row.
  const model = page.getByRole("button", { name: /^Model:/ });
  await expect(model).toBeVisible();
  // It renders the agent's *current value*, which a descriptor-driven chip could not
  // know. Showing the option's own name instead would hide real state.
  await expect(model).toHaveText(/Fast/);

  await model.click();
  const menu = page.getByRole("menu", { name: "Model" });
  await expect(menu).toBeVisible();
  // The current value is checked, read from the agent rather than from local state.
  await expect(menu.getByRole("menuitemradio", { name: "Fast" })).toBeChecked();
  await menu.getByRole("menuitemradio", { name: "Thorough" }).click();

  // The button re-renders from the response, which *is* the refreshed surface:
  // setting one option can change another's choices, so an optimistic local value
  // would be wrong exactly when it matters.
  await expect(page.getByRole("button", { name: /^Model:/ })).toHaveText(/Thorough/);
});

test("no configuration can push Send outside the composer", async ({ page }) => {
  await createSession(page);

  // The reported bug: on a phone the composer "ran off the screen". The cause was a
  // control whose intrinsic width came from content the user could not see, so this
  // asserts the invariant that replaced it — the send control stays inside the
  // composer's own box — rather than asserting a pixel width that would drift.
  const form = page.locator("form").filter({ has: page.getByLabel("Message", { exact: true }) });
  const send = page.getByRole("button", { name: "Send", exact: true });

  const formBox = await form.boundingBox();
  const sendBox = await send.boundingBox();
  expect(formBox).not.toBeNull();
  expect(sendBox).not.toBeNull();
  expect(sendBox!.x + sendBox!.width).toBeLessThanOrEqual(formBox!.x + formBox!.width + 1);

  // And again with a value selected, since selecting one is what used to widen the row.
  await page.getByRole("button", { name: /^Model:/ }).click();
  await page.getByRole("menu", { name: "Model" }).getByRole("menuitemradio", { name: "Thorough" }).click();

  const after = await send.boundingBox();
  expect(after!.x + after!.width).toBeLessThanOrEqual(formBox!.x + formBox!.width + 1);
});

test("the add menu keeps the native agent fixed after session creation", async ({
  page,
}) => {
  await createSession(page);

  // Agent lives in the `+` menu rather than on the control row: it is session-scoped,
  // where model and thinking are per-turn. Only one runner is installed, so the menu
  // reports the current agent rather than offering a switch — which is the same
  // branch a locked session takes, and is why the assertion is on the label.
  await page.getByRole("button", { name: "Add", exact: true }).click();
  const menu = page.getByRole("menu", { name: "Add" });
  await expect(menu).toBeVisible();
  await expect(menu.getByText(/Agent.*codex/)).toBeVisible();
  await page.keyboard.press("Escape");

  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("First turn");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // After a turn the agent is stated as fixed, not offered as a disabled control:
  // switching runner would hand an existing transcript to a different identity
  // policy. The row says so rather than vanishing, because a control that
  // disappears reads as a bug.
  await page.getByRole("button", { name: "Add", exact: true }).click();
  const after = page.getByRole("menu", { name: "Add" });
  await expect(after.getByText("Agent: codex")).toBeVisible();
  await expect(after.getByText("fixed")).toBeVisible();
});

test("attachments stage inside the composer rather than in a separate dropzone", async ({
  page,
}) => {
  await createSession(page);

  // The old stacked "Attach files or drop them here" strip is gone. Attaching is now
  // in the `+` menu rather than on a paperclip button: the paperclip claimed the menu
  // held only attachments, so nothing else could be put there.
  await expect(page.getByText("Attach files or drop them here.")).toHaveCount(0);
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(
    page.getByRole("menu", { name: "Add" }).getByRole("menuitem", { name: /Attach files/ }),
  ).toBeVisible();
  await page.keyboard.press("Escape");

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
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(
    page
      .getByRole("menu", { name: "Add" })
      .getByRole("menuitem", { name: /Save attachments to workspace/ }),
  ).toBeVisible();
  await page.keyboard.press("Escape");

  await staged.getByRole("button", { name: "Remove note.txt" }).click();
  await expect(staged).toHaveCount(0);
});

/**
 * The open session's row is marked as current in the sidebar.
 *
 * # Why this is an E2E spec and cannot be a unit test
 *
 * The bug it guards was a comparison that was **silently always false**. The sidebar read
 * the route param raw — `local%3Ae2e%3A…` — and compared it against `session.id`, which is
 * the decoded triple, so no row ever matched. Nothing errored, no request was malformed,
 * and the only observable symptom was the absence of a highlight nobody was looking for.
 *
 * That shape has no unit-test signature: reading the param raw still yields a string, so a
 * test over the comparison would need to already know which of the two spellings is right,
 * which is the very thing that was wrong. Only a rendered tree with a real route can
 * observe two id spaces failing to meet — which is why `lib/use-session-id-param.ts` is a
 * shared helper rather than three edits, and why this spec exists beside its unit tests.
 *
 * Asserted on `aria-current="page"` rather than on a class, because that is the promise to
 * a screen reader as well as the thing the styling hangs off: a highlight that is only
 * colour tells an assistive client nothing about where it is.
 */
test("the sidebar marks the session named by the route as current", async ({ page }) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sylph-e2e-active-"));
  const sessionId = await createSession(page, { workdir: dir });

  const sidebar = page.getByRole("complementary", { name: "Workspaces" });
  await expect(sidebar).toBeVisible();

  // Exactly one row, and it is this session's. Matched by href rather than by label,
  // because a row's text is its workdir and its agent — neither of which is unique once a
  // run has created a hundred sessions, which is the accumulation `AGENTS.md` warns
  // against counting on.
  const metadata = await (await page.request.get(`/api/v1/sessions/${encodeURIComponent(sessionId)}`)).json();
  const current = sidebar.locator(`a[href="/agents/${encodeURIComponent(metadata.session.traceAgentRef)}"]`);
  await expect(current).toHaveAttribute("aria-current", "page");

  // And only that one, or "current" would mean nothing.
  await expect(sidebar.locator('a[aria-current="page"]')).toHaveCount(1);
});
