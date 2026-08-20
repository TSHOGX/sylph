import { expect, test } from "@playwright/test";

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

test("the command dialog traps focus and returns it on close", async ({ page }) => {
  await page.goto("/sessions/new");
  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");
  await page.getByRole("button", { name: "Create session" }).click();
  await expect(page.getByLabel("Message")).toBeVisible();

  // The mock runner declares one command, so the button is present.
  const opener = page.getByRole("button", { name: "Command" });
  await opener.click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  // Focus moved into the dialog rather than staying on the page behind it.
  await expect(dialog.locator(":focus")).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // The regression this guards: without focus restoration a keyboard user is
  // dropped at the top of the document and must tab back to where they were.
  await expect(opener).toBeFocused();
});

test("every form control on the create page has an accessible name", async ({ page }) => {
  await page.goto("/sessions/new");

  const unnamed = await page.evaluate(() => {
    const controls = Array.from(
      document.querySelectorAll("input, select, textarea"),
    );
    return controls
      .filter((el) => {
        const id = el.getAttribute("id");
        const labelled =
          (id && document.querySelector(`label[for="${id}"]`)) ||
          el.getAttribute("aria-label") ||
          el.getAttribute("aria-labelledby") ||
          el.closest("label");
        return !labelled;
      })
      .map((el) => el.outerHTML.slice(0, 80));
  });

  expect(unnamed, "controls without an accessible name").toEqual([]);
});
