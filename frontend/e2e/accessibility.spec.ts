import { expect, test } from "@playwright/test";
import { createSession, gotoReady } from "./fixtures/session";

/**
 * Keyboard reachability, checked in a real browser.
 *
 * These behaviours cannot be verified by a static audit: whether a skip link
 * actually moves focus, and whether focus returns to its opener after a modal
 * closes, both depend on real focus handling.
 *
 * This is not a conformance suite. Full WCAG AA validation needs
 * assistive-technology testing and expert review.
 */
test("the skip link moves focus to main content", async ({ page }) => {
  await page.goto("/");

  // The skip link is the first focusable element, so one Tab reaches it.
  await page.keyboard.press("Tab");
  const skip = page.getByRole("link", { name: /skip to main content/i });
  await expect(skip).toBeFocused();
  // It is visually hidden until focused, so a sighted keyboard user can see it.
  await expect(skip).toBeVisible();

  await page.keyboard.press("Enter");
  await expect(page.locator("#main")).toBeVisible();
  expect(await page.evaluate(() => window.location.hash)).toBe("#main");
});

test("a control command's picker traps focus and returns it on close", async ({ page }) => {
  await createSession(page);

  // Retargeted from the old `CommandDialog`, which was removed: it ran commands from
  // the static agent descriptor, a list with no `kind`, so it would have sent
  // `/model` as a turn. The config picker is the surface that replaced it, and focus
  // handling is what this spec has always actually been about.
  // Opened from the slash menu rather than the Send button, so the opener is the
  // textarea itself — which is where a keyboard user was, and where focus has to
  // return. Driving it by keyboard is also the only way this asserts the real path.
  const composer = page.getByLabel("Message", { exact: true });
  await composer.click();
  await composer.fill("/model");
  await expect(page.getByRole("listbox", { name: "Agent commands" })).toBeVisible();
  await composer.press("Enter");

  const dialog = page.getByRole("dialog", { name: "Model" });
  await expect(dialog).toBeVisible();
  // Focus moved into the dialog rather than staying on the page behind it.
  await expect(dialog.locator(":focus")).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // The regression this guards: without focus restoration a keyboard user is dropped
  // at the top of the document and must tab back to where they were. The composer is
  // where they were, because that is what opened the picker.
  await expect(composer).toBeFocused();
});

/**
 * Every control in the create path has an accessible name.
 *
 * This used to audit the create *form*, which no longer exists — a session is
 * created by picking a directory. The check moved rather than being dropped,
 * because the picker is now the surface with the controls, and an unnamed
 * breadcrumb or folder button is the same failure the form's unnamed input would
 * have been.
 */
test("every control in the create picker has an accessible name", async ({ page }) => {
  // `gotoReady`, not `goto`: the shortcut below is discarded if it lands before the
  // shell's keydown effect has run. See the helper.
  await gotoReady(page);
  await page.keyboard.press("ControlOrMeta+Shift+o");
  await expect(page.getByRole("dialog")).toBeVisible();
  // Both surfaces of the create path, not just the first: the workspace list and
  // the folder browser are separate component trees, and the browser is the one
  // that navigates entirely by button — the crumb trail, the roots, the up
  // control. Auditing only the list would leave those uncovered.
  await expect(page.getByRole("combobox", { name: /filter workspaces/i })).toBeVisible();
  await page.getByRole("button", { name: "Browse folders..." }).click();
  // Wait for the listing, so the audit sees folder rows rather than the spinner.
  await expect(page.getByRole("listbox", { name: "Folders" })).toBeVisible();

  const unnamed = await page.evaluate(() => {
    // Buttons are included, not just fields: this surface navigates by button,
    // and a nameless one is unusable by a screen reader in the same way a
    // nameless input is. Their text content counts as the name, which is why an
    // icon-only button with no `aria-label` is what this catches.
    const controls = Array.from(
      document.querySelectorAll("input, select, textarea, button"),
    );
    return controls
      .filter((el) => {
        const id = el.getAttribute("id");
        const labelled =
          (id && document.querySelector(`label[for="${id}"]`)) ||
          el.getAttribute("aria-label") ||
          el.getAttribute("aria-labelledby") ||
          el.closest("label") ||
          (el.textContent ?? "").trim().length > 0;
        return !labelled;
      })
      .map((el) => el.outerHTML.slice(0, 80));
  });

  expect(unnamed, "controls without an accessible name").toEqual([]);
});

/**
 * Automated WCAG 2.1 A/AA audit with axe-core.
 *
 * This checks the rules a machine can check: colour contrast, accessible names,
 * ARIA validity, landmark structure, and heading order. It is a floor, not a
 * conformance claim — axe covers roughly a third of WCAG criteria, and the rest
 * (focus order making sense, alternatives being *meaningful*) needs
 * assistive-technology testing and expert review. The keyboard tests above cover
 * what axe cannot.
 */
async function audit(page: import("@playwright/test").Page, context?: string) {
  const { default: AxeBuilder } = await import("@axe-core/playwright");
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();

  const violations = results.violations.map((violation) => ({
    id: violation.id,
    impact: violation.impact,
    help: violation.help,
    // Selector *and* the failure summary axe computed. The selector alone names a
    // class soup that can match several elements, and for contrast failures the
    // summary carries the measured ratio and the two colours — which is the
    // difference between fixing the right element and guessing at one.
    nodes: violation.nodes.slice(0, 3).map((node) => ({
      target: node.target.join(" "),
      html: node.html.slice(0, 160),
      summary: node.failureSummary?.replace(/\s+/g, " ").slice(0, 240),
    })),
  }));

  expect(
    violations,
    `axe violations${context ? ` on ${context}` : ""}: ${JSON.stringify(violations, null, 2)}`,
  ).toEqual([]);
}

test("the start screen has no automated accessibility violations", async ({ page }) => {
  // With a session present, so the recents list is in scope rather than only the
  // static chrome. Earlier specs in this serial run have created sessions.
  await createSession(page);
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await audit(page, "start screen");
});

/**
 * The first-run guide, which the audit above never sees.
 *
 * It is a different subtree — a numbered list, the step markers, and a button
 * inside prose — and it is the *only* thing a brand-new user looks at. Reaching it
 * needs an empty listing, which a shared journal cannot provide, so the listing is
 * intercepted; the markup being audited is the client's either way.
 */
test("the first-run guide has no automated accessibility violations", async ({ page }) => {
  await page.route("**/api/v1/sessions?*", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );
  await page.goto("/");
  await expect(page.getByRole("region", { name: "Getting started" })).toBeVisible();
  await audit(page, "start screen, first run");
});

test("the create picker has no automated accessibility violations", async ({ page }) => {
  await gotoReady(page);
  await page.keyboard.press("ControlOrMeta+Shift+o");
  await expect(page.getByRole("dialog")).toBeVisible();

  // Audited with a list actually open, in both states. A filterable list is the
  // combobox pattern, and the invalid-ARIA failure that pattern exists to avoid —
  // an `<input>` inside a `role="menu"` — is only reachable while the surface is
  // rendered. A static read does not catch it.
  await expect(page.getByRole("combobox", { name: /filter workspaces/i })).toBeVisible();
  await audit(page, "workspace picker");

  await page.getByRole("button", { name: "Browse folders..." }).click();
  await expect(page.getByRole("listbox", { name: "Folders" })).toBeVisible();
  await audit(page, "folder browser");
});

test("the conversation canvas has no automated accessibility violations", async ({
  page,
}) => {
  await createSession(page);

  // Audit with real content: tool calls, a diff, and a failed command are the
  // parts with the most custom markup and the most colour.
  await page.getByLabel("Message", { exact: true }).fill("exercise tools please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // Both sidebars open, so the tree and the panes are in scope too.
  await page.keyboard.press("ControlOrMeta+Backslash");
  await expect(page.getByRole("complementary", { name: "Work panel" })).toBeVisible();

  await audit(page, "conversation canvas with tool calls");
});

/**
 * A delegation tree, audited with real nesting rather than one card.
 *
 * Nested landmarks are the risk here: each delegation is a `region`, so a depth-3
 * tree nests three of them, and a `region` with no accessible name is a landmark a
 * screen reader announces as nothing. The failed branch also auto-expands, which
 * puts the danger colours inside a `bg-raised` card — a contrast pairing that does
 * not occur anywhere else in the timeline.
 */
test("a nested delegation tree has no automated accessibility violations", async ({
  page,
}) => {
  const {runExecution,openExecutionGroups}=await import("./fixtures/execution");
  const {sessionId,turnId}=await runExecution(page,"delegate this please");
  await openExecutionGroups(page,sessionId,turnId);
  const explore=page.getByRole("region",{name:"Explore delegation"});
  await explore.getByRole("button").first().click();
  await audit(page,"delegation detail beside the conversation");
  await page.getByRole("button",{name:"All subagents"}).click();
  await audit(page,"delegation catalogue");
});

/**
 * The composer *while a turn runs*, which is a different colour state.
 *
 * The composer is disabled mid-turn, and it used to express that with
 * `opacity-60` on its container. Opacity composites the whole subtree, so every
 * descendant lost contrast at once: `text-gray-600` measured 2.87:1 and the Stop
 * button 3.7:1, against the 4.5:1 WCAG 1.4.3 requires — and Stop is the only
 * control that matters at that moment.
 *
 * The audit above cannot catch this, because it waits for the reply before
 * auditing and the default runner finishes in under a second. It went unnoticed
 * until a faster test fixture happened to land the audit mid-turn. `paced` spaces
 * its stages a second apart, which makes the live state hold still long enough to
 * assert on deliberately rather than by luck.
 */
test("the composer has no accessibility violations while a turn is running", async ({
  page,
}) => {
  await createSession(page);

  await page.getByLabel("Message", { exact: true }).fill("paced please");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // Stop replaces Send only while the turn is live, so its presence *is* the
  // proof the audit runs in the state being tested.
  await expect(page.getByRole("button", { name: /^Stop/ })).toBeVisible();

  await audit(page, "composer during a live turn");
});

test("every overlay has no automated accessibility violations", async ({ page }) => {
  await createSession(page);

  for (const [combo, name] of [
    ["ControlOrMeta+k", "search overlay"],
    ["ControlOrMeta+p", "command palette"],
    // The history browser was absent from this loop while it was the one overlay
    // rendering a paging button and a status line *inside* its `role="listbox"` —
    // `aria-required-children`, the same invalid-ARIA class as an `<input>` in a
    // `role="menu"`. It is also the overlay a machine without TraceDB sees in its
    // degraded state, which is where the non-option children appear.
    ["ControlOrMeta+Shift+k", "history browser"],
    ["ControlOrMeta+Comma", "settings panel"],
  ] as const) {
    await page.keyboard.press(combo);
    await expect(page.getByRole("dialog")).toBeVisible();
    await audit(page, name);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toBeHidden();
  }
});

/**
 * The adopt picker, which the loop above cannot reach.
 *
 * That loop is keyed by shortcut, and adopting has no default binding — it is reached
 * through the palette. So it was the one overlay never audited, which is precisely the
 * position the history browser was in when it shipped `aria-required-children`: a
 * non-option child inside a `role="listbox"`, invisible to every spec because no audit
 * ever opened it.
 *
 * Audited with rows present rather than empty, because the states differ in ARIA and only
 * one of them has a listbox at all: `SelectScroll` claims the role only when it has options
 * to contain, so an empty listing would audit the branch where the hazard cannot occur.
 * Verified sensitive by putting a `button` inside that listbox, which this reports as
 * `aria-required-children` — a decorative `<p>` there does not, so "no violations" here is a
 * claim about interactive children specifically.
 */
test("the adopt picker has no automated accessibility violations", async ({ page }) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({
      json: {
        sessions: [
          {
            agent: "e2e",
            nativeId: "11111111-2222-3333-4444-555555555555",
            workdir: "/tmp",
            fidelity: "blocks",
            id: "local:e2e:11111111-2222-3333-4444-555555555555",
            title: "Fix the SSE reconnect loop",
            eventCount: 42,
          },
          {
            agent: "e2e",
            nativeId: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            workdir: "/tmp/other",
            fidelity: "blocks",
            id: "local:e2e:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            blocked: "outsideRoots",
          },
        ],
      },
    }),
  );

  await createSession(page);

  await page.keyboard.press("ControlOrMeta+p");
  const palette = page.getByRole("dialog");
  await expect(palette).toBeVisible();
  await palette.getByRole("combobox").fill("adopt");
  await expect(palette.getByRole("option").first()).toContainText(/adopt/i);
  await page.keyboard.press("Enter");

  const picker = page.getByRole("dialog", { name: "Adopt an existing session" });
  await expect(picker).toBeVisible();
  // Waits for a row, so the audit provably runs against the listbox branch rather than
  // against the loading state that precedes it.
  await expect(picker.getByRole("option").first()).toBeVisible();

  await audit(page, "adopt picker");
});

/**
 * A picker's own field must not become the node focus is restored to.
 *
 * React's `autoFocus` fires during the commit, before `useDialog`'s mount effect
 * records where focus came from — so a field declared that way was captured *as* the
 * opener, and closing the overlay "restored" focus to a node that was unmounting,
 * dropping it on `<body>` (WCAG 2.4.3). The three overlays all declared it, and no
 * spec caught it because the two overlays whose focus restoration *was* covered — the
 * settings panel and the config picker — have no auto-focusing control at all.
 *
 * The fix is `SelectFilter`'s deferred focus, which lands after every ancestor's
 * effects. This asserts the outcome rather than the mechanism: focus is in the field
 * on open, and back on the composer on close.
 */
test("an overlay's filter takes focus without becoming the node focus returns to", async ({
  page,
}) => {
  await createSession(page);

  const composer = page.getByLabel("Message", { exact: true });
  await composer.click();
  await expect(composer).toBeFocused();

  for (const [combo, name] of [
    ["ControlOrMeta+p", /command query/i],
    ["ControlOrMeta+k", /search query/i],
    ["ControlOrMeta+Shift+k", /filter this workspace/i],
  ] as const) {
    await page.keyboard.press(combo);
    await expect(page.getByRole("combobox", { name })).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toBeHidden();
    await expect(composer).toBeFocused();
  }
});

/**
 * The two patterns a choice list presents, audited open.
 *
 * A `role="menu"` may only contain menu items, so the filter field added for long
 * catalogues cannot live in one: axe reports it, and a screen reader is handed a
 * control its container says cannot exist. The surface therefore switches to the
 * combobox pattern when it is filterable. That switch is exactly the kind of thing a
 * static read of the code will not catch and a click-through will not notice, which
 * is why both states are audited with a list actually open.
 */
test("both a menu and a filterable list are valid ARIA while open", async ({ page }) => {
  await createSession(page);

  await page.getByRole("button", { name: /^Model:/ }).click();
  await expect(page.getByRole("menu", { name: "Model" })).toBeVisible();
  await audit(page, "a short choice list (menu pattern)");
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: /^Provider model:/ }).click();
  await expect(page.getByRole("combobox", { name: /filter provider model/i })).toBeFocused();
  await audit(page, "a long choice list (combobox pattern)");
  await page.keyboard.press("Escape");

  // And the dialog form of the same list, which a control command opens. It reaches
  // the filterable path through a different surface — an `Overlay` that owns its own
  // dismissal — so its roles are worth auditing separately from the popover's.
  const composer = page.getByLabel("Message", { exact: true });
  await composer.click();
  await composer.fill("/model");
  await composer.press("Enter");
  await expect(page.getByRole("dialog", { name: "Model" })).toBeVisible();
  await audit(page, "a control command's picker");
});
