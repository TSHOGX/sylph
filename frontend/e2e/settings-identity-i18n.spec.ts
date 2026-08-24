import { expect, test } from "@playwright/test";
import { gotoReady } from "./fixtures/session";

/**
 * Settings behaviour that only exists end to end.
 *
 * Three things here cannot be covered by a unit test: that the SOUL.md toggle and
 * its preview reflect what the *backend* resolved, that switching language
 * repaints the UI and updates `<html lang>`, and that a browser-reserved combo is
 * refused by the recorder rather than stored.
 *
 * The E2E backend runs with no `SOUL.md` — its runners directory is the fixture
 * tree, which has none — so the identity assertions here are about the
 * unavailable path. That is the honest coverage this harness can give: a spec that
 * wrote a SOUL.md into the fixture tree would be asserting against a file it had
 * just created rather than against resolution, which the backend's own
 * integration tests already pin with a real one.
 */

async function openSettings(page: import("@playwright/test").Page) {
  // Wait for the shell before pressing the shortcut. The binding is registered by a
  // client component, so a keypress sent between navigation and hydration reaches a
  // document with no handler for it — the key is swallowed and the dialog never
  // opens. That made this helper fail only under load, which reads as flakiness
  // rather than as a race, and only after a `reload()`.
  //
  // Matched by role rather than by name: this helper is also used with the UI in
  // Chinese, where every accessible name is translated. A label here would pin the
  // readiness check to one locale and fail in the other.
  await expect(page.getByRole("banner").getByRole("button").first()).toBeVisible();
  await page.keyboard.press("ControlOrMeta+Comma");
  const settings = page.getByRole("dialog");
  await expect(settings).toBeVisible();
  return settings;
}

test("the identity section reports that no SOUL.md was resolved", async ({ page }) => {
  await gotoReady(page);
  const settings = await openSettings(page);

  await settings.getByRole("button", { name: "Identity" }).click();

  // Both choices stay operable with no SOUL.md, so the stored preference remains
  // readable — disabling them would hide what the user had chosen.
  const sylph = settings.getByRole("button", { name: /Append SOUL\.md/ });
  const native = settings.getByRole("button", { name: /agent as it comes/ });
  await expect(sylph).toBeEnabled();
  await expect(native).toBeEnabled();
  await expect(sylph).toHaveAttribute("aria-pressed", "true");

  await expect(settings.getByText(/No SOUL\.md was found/)).toBeVisible();

  await native.click();
  await expect(native).toHaveAttribute("aria-pressed", "true");
  await expect(sylph).toHaveAttribute("aria-pressed", "false");

  // Persisted, not merely toggled: this is a default for every session created
  // afterwards, so it has to survive a reload.
  await page.reload();
  await openSettings(page);
  await settings.getByRole("button", { name: "Identity" }).click();
  await expect(settings.getByRole("button", { name: /agent as it comes/ })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
});

test("switching language repaints the UI and retags the document", async ({ page }) => {
  await gotoReady(page);
  const settings = await openSettings(page);

  await expect(page.locator("html")).toHaveAttribute("lang", "en");

  // The picker's own labels are endonyms and deliberately untranslated, which is
  // what lets a user who cannot read the current language find their way back.
  await settings.getByRole("button", { name: "中文", exact: true }).click();

  // A screen reader picks its voice from `lang`, so this is the assertion that
  // matters most: a Chinese UI still tagged `en` is read as English attempting
  // Chinese.
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hans");
  await expect(settings.getByRole("heading", { level: 3, name: "设置" })).toBeVisible();

  // Chrome outside the panel translates too — this is the check that catches an
  // extraction that stopped at the settings dialog.
  await page.keyboard.press("Escape");
  await expect(page.getByRole("navigation", { name: "按工作区分组的会话" })).toBeVisible();

  await openSettings(page);
  await settings.getByRole("button", { name: "English", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await expect(settings.getByRole("heading", { level: 3, name: "Settings" })).toBeVisible();
});

/**
 * The Artifacts pane, in the other language.
 *
 * Its own spec rather than more assertions above, because the gap it covers is a
 * *whole surface* left out of the extraction rather than one missed string: the
 * file pane shipped with its labels hardcoded while the shell around it
 * translated, and every existing i18n assertion looked at the shell. A pane
 * reached by a shortcut and rendered by a lazy chunk is exactly the surface a
 * catalogue sweep misses, so the check is on the pane's own controls.
 *
 * Asserting the accessible names, not visible text: they are what a screen reader
 * announces and what an untranslated `label` prop leaves in English even when the
 * body text around it changes.
 */
test("the file pane translates, including its accessible names", async ({ page }) => {
  await gotoReady(page);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "中文", exact: true }).click();
  await page.keyboard.press("Escape");

  await page.keyboard.press("ControlOrMeta+Backslash");
  const pane = page.getByRole("complementary", { name: "产物" });
  await expect(pane).toBeVisible();

  // With no session open the pane is its empty state, which is the one part of it
  // that renders without a workdir — and it was hardcoded English too.
  await expect(pane.getByText("未打开会话")).toBeVisible();
});

/**
 * The defect this whole change started from.
 *
 * `Mod+N` shipped as the default for "new session" and in a real browser it opens
 * a new window instead — `preventDefault` cannot reach it. Note that Playwright's
 * Chromium does *not* open a window here, which is exactly why the suite passed
 * with the broken default for as long as it did: this spec asserts on the
 * recorder's refusal, which is observable, rather than on browser chrome, which is
 * not.
 */
test("the shortcut recorder refuses a combo the browser keeps for itself", async ({ page }) => {
  await gotoReady(page);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "Shortcuts" }).click();

  const binding = settings.getByRole("button", { name: /^New session shortcut/ });
  const before = await binding.getAttribute("aria-label");

  await binding.click();
  await page.keyboard.press("ControlOrMeta+n");

  // Refused with a reason naming what the browser does instead. The recorder
  // deliberately stays open so the next keypress is the retry, which is why the
  // untouched binding is asserted through `aria-label` rather than the caption —
  // the caption is showing the recording prompt at this point, by design.
  await expect(settings.getByRole("alert")).toContainText(/reserved by the browser/i);
  await expect(binding).toHaveAttribute("aria-label", before ?? "");

  // A bindable combo in the same recording session still works, so the refusal
  // is specific rather than the recorder having broken.
  await page.keyboard.press("ControlOrMeta+Shift+u");
  await expect(binding).toHaveText(/⇧U|Shift\+U/);
});

test("no default binding is one the browser would swallow", async ({ page }) => {
  await gotoReady(page);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "Shortcuts" }).click();

  // Reading the rendered captions rather than the stored map: this is what a user
  // sees and tries to press, and a caption showing a combo that cannot fire is
  // the failure being guarded against.
  const captions = await settings.getByRole("list").getByRole("button").allTextContents();
  for (const caption of captions) {
    expect(caption, `${caption} is reserved by the browser`).not.toMatch(
      /^(⌘|Ctrl\+)(⇧|Shift\+)?[NTWQ]$/,
    );
  }
});
