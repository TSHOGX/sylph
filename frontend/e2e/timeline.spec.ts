import { expect, test } from "./fixtures/test";
import { installTraceFixture, message, openTrace } from "./fixtures/trace";

test("execution boundary availability is independent of retained prose", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Readable without a fabricated turn"),
  ]);
  state.mainHeader.ownExecution = {
    reference: null,
    state: { kind: "observation_ended", lastObservedAtMs: 123 },
    durations: [],
  };
  await openTrace(page);
  await expect(
    page.getByText("Readable without a fabricated turn", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(/Last observed working · outcome unknown/),
  ).toBeVisible();
  await expect(page.getByText(/^\d+ ms$/)).toHaveCount(0);
});

test("keyboard movement across inspector tabs does not activate a new destination", async ({
  page,
}) => {
  await installTraceFixture(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  const tab = page.getByRole("tab", { name: "Details", exact: true });
  await tab.focus();
  await page.keyboard.press("ArrowRight");
  await expect(
    page.getByRole("tab", { name: "Files", exact: true }),
  ).toBeFocused();
  await expect(tab).toHaveAttribute("aria-selected", "true");
  await expect(
    page.getByRole("heading", { name: "Record coverage" }),
  ).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(
    page.getByText("No file context is declared for this agent.", {
      exact: true,
    }),
  ).toBeVisible();
});

test("localized inspector labels never become navigation or tab identifiers", async ({ page }) => {
  await installTraceFixture(page);
  await page.addInitScript(() => localStorage.setItem("sylph-web.settings.v1", JSON.stringify({ locale: "zh", showLeftSidebar: false, showRightSidebar: false })));
  await openTrace(page);
  await page.getByRole("button", { name: "代理", exact: true }).click();
  await expect(page.getByRole("tab", { name: "代理", exact: true })).toHaveAttribute("aria-selected", "true");
  await page.getByRole("tab", { name: "文件", exact: true }).click();
  await expect(page.getByText("此代理未声明文件上下文。", { exact: true })).toBeVisible();
  await expect(page.getByRole("tab", { name: "文件", exact: true })).toHaveAttribute("aria-selected", "true");
});

test("browser Find keeps its reserved shortcut while product search remains discoverable", async ({ page }) => {
  await installTraceFixture(page); await openTrace(page);
  const intercepted = await page.evaluate(() => {
    const event = new KeyboardEvent("keydown", { key: "f", ctrlKey: true, metaKey: true, bubbles: true, cancelable: true }); document.dispatchEvent(event); return event.defaultPrevented;
  });
  expect(intercepted).toBe(false);
  await expect(page.getByRole("button", { name: "Find in this agent", exact: true })).toBeVisible();
});
