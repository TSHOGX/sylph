import { expect, test } from "@playwright/test";
import {
  installTraceFixture,
  message,
  openTrace,
  MAIN_AGENT,
} from "./fixtures/trace";

test("prepending a consistent history page preserves the visible object and pixel offset", async ({
  page,
}) => {
  await installTraceFixture(
    page,
    Array.from({ length: 400 }, (_, index) => message(index)),
  );
  await openTrace(page, MAIN_AGENT, `${MAIN_AGENT}-anchor-360`);
  const scroller = page.locator("[data-trace-scroller]");
  await scroller.evaluate((element) => {
    element.scrollTop = 0;
  });
  const anchor = page.locator(`[data-trace-anchor="${MAIN_AGENT}-anchor-355"]`);
  await expect(anchor).toBeVisible();
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(requestAnimationFrame);
  });
  const before = (await anchor.boundingBox())!.y;
  const earlier = page.waitForResponse((response) =>
    response.url().includes("/history?"),
  );
  await page
    .getByRole("button", { name: "Earlier history", exact: true })
    .click();
  await expect
    .poll(async () => (await (await earlier).json()).items[0].anchor)
    .toBe(`${MAIN_AGENT}-anchor-315`);
  await expect
    .poll(async () => Math.abs((await anchor.boundingBox())!.y - before))
    .toBeLessThanOrEqual(2);
});

test("a large selected history mounts only the served row budget", async ({
  page,
}) => {
  await installTraceFixture(
    page,
    Array.from({ length: 800 }, (_, index) => message(index)),
  );
  const response = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === `/api/v1/trace/agents/${MAIN_AGENT}`,
  );
  await openTrace(page);
  const served = await (await response).json();
  expect(
    await page.locator("[data-trace-address]").count(),
  ).toBeLessThanOrEqual(served.window.limits.maxMountedRows);
  await page.getByRole("button", { name: "Paged reading off" }).click();
  await expect(
    page.getByRole("button", { name: "Paged reading on" }),
  ).toHaveAttribute("aria-pressed", "true");
  expect(
    await page.locator("[data-trace-address]").count(),
  ).toBeLessThanOrEqual(served.window.limits.maxMountedRows);
});

test("opening a payload is explicit and unrelated view revisions keep its immutable body", async ({
  page,
}) => {
  const row = message(0, "Short retained preview");
  if (row.content.kind !== "message") throw new Error("message fixture");
  row.content.parts[0].previewTruncated = true;
  const state = await installTraceFixture(page, [row]);
  state.bodies.set("body-0", "The retained complete body");
  await openTrace(page);
  expect(state.reads.filter((read) => read.includes("/payloads/")).length).toBe(
    0,
  );
  await page
    .getByRole("button", { name: /Read recorded conversation result/i })
    .click();
  await expect(
    page.getByText("The retained complete body", { exact: true }),
  ).toBeVisible();
  expect(state.reads.filter((read) => read.includes("/payloads/")).length).toBe(
    1,
  );
  state.revision = 2;
  await expect(
    page.getByText("History updated", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Refresh around this item" }).click();
  await expect
    .poll(
      () =>
        state.reads.filter((read) =>
          /^GET \/api\/v1\/trace\/agents\/opaque-main-agent(?:\?|$)/.test(read),
        ).length,
    )
    .toBeGreaterThan(1);
  await expect(
    page.getByText("The retained complete body", { exact: true }),
  ).toBeVisible();
  expect(state.reads.filter((read) => read.includes("/payloads/")).length).toBe(
    1,
  );
});

test("paused history keeps visible content until the reader accepts a correction", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Original retained interpretation"),
  ]);
  await openTrace(page);
  await page.getByRole("button", { name: "Evidence and details" }).focus();
  state.rows = [message(0, "Corrected retained interpretation")];
  state.revision = 2;
  await expect(
    page.getByText("History updated", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Original retained interpretation", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Corrected retained interpretation", { exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Refresh around this item" }).click();
  await expect(
    page.getByText("Corrected retained interpretation", { exact: true }),
  ).toBeVisible();
});

test("virtualization retains the focused reading row outside the visible window", async ({ page }) => {
  await installTraceFixture(page, Array.from({ length: 120 }, (_, index) => message(index)));
  await openTrace(page, MAIN_AGENT, `${MAIN_AGENT}-anchor-80`);
  const target = page.locator(`[data-trace-anchor="${MAIN_AGENT}-anchor-80"]`).getByRole("button", { name: "Evidence and details" });
  await target.focus();
  await page.locator("[data-trace-scroller]").evaluate((element) => { element.scrollTop = element.scrollHeight; });
  await expect(target).toBeAttached(); await expect(target).toBeFocused();
});
