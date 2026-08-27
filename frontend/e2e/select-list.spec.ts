import { expect, test } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * A long choice list: scrollable, filterable, and selectable.
 *
 * # Why this file exists
 *
 * Every reported symptom here was invisible to the existing suite, and for one
 * reason: the E2E fixture declared model options *two at a time*. A two-item list
 * fits any panel, so nothing could observe a list too long to scroll; it is below
 * the filter threshold, so nothing could observe the filter at all. The fixture now
 * declares seventeen, which is the range real agents report.
 *
 * # What each spec pins
 *
 * The three reported failures were: a catalogue longer than the panel could not be
 * scrolled to reach the rest, clicking a row changed nothing on screen, and the
 * filter field never received focus so typing went nowhere. The first was a panel
 * that was `overflow-hidden` with no scrolling child. The other two shared a cause —
 * a cursor counted over one list and rendered over another, and an `autoFocus` that
 * fired while the panel was still `visibility: hidden` and therefore unfocusable.
 *
 * These assert the invariants those fixes established rather than their mechanisms:
 * every option is reachable, a click commits the value the row displays, and the
 * field has focus on open. A spec that asserted a panel height or a scroll offset
 * would fail on the next padding change while saying nothing about usability.
 */

/** The composer chip for the long catalogue. Named for its option, then its value. */
const TRIGGER = /^Provider model:/;

test("a catalogue past the threshold opens filtered, focused, and scrollable", async ({
  page,
}) => {
  await createSession(page);

  await page.getByRole("button", { name: TRIGGER }).click();

  // A filterable list is a combobox, not a menu: `role="menu"` may only contain menu
  // items, so an `<input>` inside one is invalid ARIA. The role switch is the fix,
  // and asserting it here is what stops a future change from nesting the two.
  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  await expect(filter).toBeVisible();

  // The reported bug: focus stayed on the trigger, because React's `autoFocus` fires
  // at mount and the panel is `visibility: hidden` until it has been measured — and
  // hidden elements cannot take focus. Every keystroke went to the trigger instead,
  // so the field looked dead.
  await expect(filter).toBeFocused();

  const options = page.getByRole("listbox", { name: /provider model/i });
  await expect(options.getByRole("option")).toHaveCount(17);

  // The other reported bug: the panel was `overflow-hidden` with no scrolling child,
  // so a list taller than it was cut off with nothing to scroll. Asserted as the
  // invariant rather than as a pixel height — the last option must be reachable.
  const last = options.getByRole("option", { name: "DeepSeek V4" });
  await last.scrollIntoViewIfNeeded();
  await expect(last).toBeInViewport();
});

test("typing filters the list, and matches the agent's description as well as the label", async ({
  page,
}) => {
  await createSession(page);
  await page.getByRole("button", { name: TRIGGER }).click();

  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  const options = page.getByRole("listbox", { name: /provider model/i }).getByRole("option");

  await filter.fill("kimi");
  await expect(options).toHaveCount(2);

  // The hint is the agent's own description of a choice, which for a model catalogue
  // is the provider — the word a user is most likely to remember. Matching the label
  // alone would find nothing for "openai", since no model is called that.
  await filter.fill("openai");
  await expect(options).toHaveCount(3);
  await expect(options.first()).toContainText("GPT-5");

  await filter.fill("zzzz");
  await expect(options).toHaveCount(0);
  await expect(page.getByText("No matches")).toBeVisible();

  // Clearing restores the whole catalogue rather than leaving the last result set.
  await filter.fill("");
  await expect(options).toHaveCount(17);
});

test("clicking a filtered row selects that row's value", async ({ page }) => {
  await createSession(page);
  await page.getByRole("button", { name: TRIGGER }).click();

  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  await filter.fill("haiku");

  const row = page
    .getByRole("listbox", { name: /provider model/i })
    .getByRole("option", { name: "Haiku 4.5" });
  await row.click();

  // The reported symptom was that this did nothing visible. The cause was two index
  // spaces: the renderer numbered every row including group headings while the cursor
  // counted only selectable ones, so the highlight, `aria-activedescendant`, and the
  // committed value could each point at a different row — and with a filter applied,
  // at a row that was not even on screen.
  //
  // Asserted on the trigger, which re-renders from the agent's response rather than
  // from local state: setting one option can change another's choices, so an
  // optimistic value would be wrong exactly when it matters.
  await expect(page.getByRole("button", { name: TRIGGER })).toHaveText(/Haiku 4\.5/);
});

test("the keyboard can filter and commit without touching the mouse", async ({ page }) => {
  await createSession(page);
  await page.getByRole("button", { name: TRIGGER }).click();

  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  await filter.fill("qwen");

  // Space types rather than commits. A menu row is activated by Space, but a
  // filterable list has a text field — and model descriptions are full of spaces, so
  // a Space that committed made the field unusable for exactly the lists that need
  // one. The value surviving in the field is what proves it typed.
  await filter.press("Space");
  await expect(filter).toHaveValue("qwen ");

  await filter.fill("qwen");
  await filter.press("Enter");
  await expect(page.getByRole("button", { name: TRIGGER })).toHaveText(/Qwen 3 Max/);
});

test("Escape closes a filterable list and returns focus to its trigger", async ({ page }) => {
  await createSession(page);

  const trigger = page.getByRole("button", { name: TRIGGER });
  await trigger.click();

  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  await expect(filter).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(filter).toBeHidden();

  // Without this a keyboard user is dropped at the top of the document (WCAG 2.4.3).
  // It is also the specific regression risk of moving focus into a text field: the
  // old code left focus on the trigger, so restoration was accidentally correct.
  await expect(trigger).toBeFocused();
});

test("a short list stays a menu, so the filter is not imposed on two-value knobs", async ({
  page,
}) => {
  await createSession(page);

  // The same screen carries both, which is the point: `Model` declares two options
  // and `Provider model` fourteen, so the threshold is observable rather than
  // asserted in the abstract.
  await page.getByRole("button", { name: /^Model:/ }).click();

  const menu = page.getByRole("menu", { name: "Model" });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("combobox")).toHaveCount(0);
  await expect(menu.getByRole("menuitemradio", { name: "Fast" })).toBeChecked();

  // And the menu pattern's own keys still work, including the Space that the
  // filterable list must not consume.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Space");
  await expect(page.getByRole("button", { name: /^Model:/ })).toHaveText(/Thorough/);
});

test("a long list opens on the current value rather than at the top", async ({ page }) => {
  await createSession(page);

  // The fixture's `currentValue` is `sonnet-5`, tenth from last. A catalogue that
  // opened at row zero would put the user's own choice off-screen, which is the
  // difference between a list that can be scanned and one that has to be searched.
  await page.getByRole("button", { name: TRIGGER }).click();

  const current = page
    .getByRole("listbox", { name: /provider model/i })
    // Not `exact`: a row's accessible name is its label *and* its hint, so this one
    // announces as "Sonnet 5 Anthropic". That is deliberate — the provider is what
    // distinguishes otherwise similar entries, and a screen reader should hear it.
    .getByRole("option", { name: "Sonnet 5" });
  await expect(current).toBeInViewport();
  await expect(current).toHaveAttribute("aria-selected", "true");
});

/**
 * The dialog form of the same list.
 *
 * A control command has no trigger element to anchor a popover to — it is invoked
 * from the composer *text* — so it opens a modal instead. That is a different surface
 * with different focus ownership: the `Overlay` owns dismissal, which is why the list
 * must not claim Escape there, and the field is focusable from its first paint rather
 * than after a measurement pass. Same list, so the same rows and the same cursor.
 */
test("a control command onto a long catalogue opens the same filterable list", async ({
  page,
}) => {
  await createSession(page);

  const composer = page.getByLabel("Message");
  await composer.click();
  await composer.fill("/provider");
  await expect(page.getByRole("listbox", { name: "Agent commands" })).toBeVisible();
  await composer.press("Enter");

  const dialog = page.getByRole("dialog", { name: "Provider model" });
  await expect(dialog).toBeVisible();

  const filter = dialog.getByRole("combobox", { name: /filter provider model/i });
  await expect(filter).toBeFocused();

  await filter.fill("mistral");
  const rows = dialog.getByRole("option");
  await expect(rows).toHaveCount(1);
  await rows.first().click();

  await expect(dialog).toBeHidden();
  await expect(page.getByRole("button", { name: TRIGGER })).toHaveText(/Mistral Large 3/);
  // Nothing was sent: a control command opens a picker, it is never a turn.
  await expect(composer).toHaveValue("");
});

test("Escape closes the dialog form, which the list must not swallow", async ({ page }) => {
  await createSession(page);

  const composer = page.getByLabel("Message");
  await composer.click();
  await composer.fill("/provider");
  await composer.press("Enter");

  const dialog = page.getByRole("dialog", { name: "Provider model" });
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");

  // The list does not own this surface, so it must let Escape bubble to the
  // `Overlay`. Claiming the key unconditionally — stopping propagation with no
  // `onClose` to call — swallowed it and left the dialog undismissable by keyboard.
  await expect(dialog).toBeHidden();
  await expect(composer).toBeFocused();
});

/**
 * A refused set is reported, not silently ignored.
 *
 * This is the spec the reported bug actually needed. The picker was correct and the
 * agent was refusing: `pi`'s adapter sent `set_model(model=…)` where the CLI wanted
 * `provider` + `modelId`, so every selection came back a failed RPC. With no error
 * path in the client, the chip kept its old value and nothing was said — which is
 * indistinguishable from a dead control, and sent the investigation to the UI layer
 * for two rounds.
 *
 * So the invariant is: whatever the agent does, the user finds out. The fixture
 * refuses one specific value to make that observable.
 */
test("an agent that refuses a config value says so instead of appearing broken", async ({
  page,
}) => {
  await createSession(page);
  await page.getByRole("button", { name: TRIGGER }).click();

  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  await filter.fill("unavailable");
  await page
    .getByRole("listbox", { name: /provider model/i })
    .getByRole("option", { name: /Unavailable Model/ })
    .click();

  // The refusal reaches the user, carrying the agent's own words — it knows why it
  // declined and that is what tells them to pick something else.
  const alert = page.getByRole("alert").filter({ hasText: /could not apply that setting/i });
  await expect(alert).toBeVisible();
  await expect(alert).toContainText(/refuses that value/i);

  // And the control still reports what the agent actually holds, rather than the
  // value this client optimistically believed it set.
  await expect(page.getByRole("button", { name: TRIGGER })).toHaveText(/Sonnet 5/);
});

/**
 * Two providers, one model name: exactly one row reads as current.
 *
 * The reported symptom, directly. `pi` 0.84.3 offers two "Claude Opus 5" and two each
 * of "DeepSeek V4 Pro" and "GLM-5.2" — the same model id from different providers — so
 * a config value of the bare id was not unique. Every producer computes
 * `checked: choice.value === currentValue`, which is a *filter*: it marked both rows,
 * and neither was identifiably the real one.
 *
 * Two layers had to change and both are asserted here: the adapter now issues
 * provider-qualified values, and the client enforces one current row per single-select
 * group regardless of what an agent reports. The second is what protects against the
 * next agent, including ones written by other people.
 */
test("two providers offering one model name check exactly one row", async ({ page }) => {
  await createSession(page);
  await page.getByRole("button", { name: TRIGGER }).click();

  const filter = page.getByRole("combobox", { name: /filter provider model/i });
  await filter.fill("GLM");

  const rows = page.getByRole("listbox", { name: /provider model/i }).getByRole("option");
  await expect(rows).toHaveCount(2);

  // Same label, told apart by the agent's own description — which is the only thing
  // distinguishing them, so it is load-bearing rather than decorative.
  await expect(rows.nth(0)).toContainText("ikun-glm");
  await expect(rows.nth(1)).toContainText("huaweicloud");

  // Selecting the second provider must reach *that* provider, not whichever one a
  // by-id lookup happened to resolve first.
  await rows.nth(1).click();
  await expect(page.getByRole("button", { name: TRIGGER })).toHaveText(/GLM-5\.2/);

  await page.getByRole("button", { name: TRIGGER }).click();
  await page.getByRole("combobox", { name: /filter provider model/i }).fill("GLM");

  // The bug: both rows checked at once. Exactly one may be current, and it must be
  // the one that was actually chosen. Counted across the whole open list rather than
  // asserted row by row, so an extra checkmark anywhere fails rather than only one in
  // a position this spec happened to name.
  const selected = page
    .getByRole("listbox", { name: /provider model/i })
    .getByRole("option", { selected: true });
  await expect(selected).toHaveCount(1);
  await expect(selected).toContainText("huaweicloud");
});
