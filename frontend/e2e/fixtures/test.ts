import { test as base, type Page } from "@playwright/test";

export { expect, type Locator, type Page } from "@playwright/test";

/** Leave the real document while its context can still deliver keepalive requests. */
export async function finishDocument(page: Page) {
  if (page.isClosed()) return;
  // Context disposal kills Chromium documents without pagehide. A normal
  // navigation runs the product's release path. Network idle is a teardown
  // barrier here, so the context cannot abort those requests after navigation.
  await page.goto("about:blank", { waitUntil: "networkidle" });
}

export const test = base.extend({
  page: async ({ page }, run) => {
    await run(page);
    await finishDocument(page);
  },
});
