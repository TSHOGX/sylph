import { expect, test } from "./fixtures/test";
import { installTraceFixture, message, openTrace } from "./fixtures/trace";

test("transport EOF does not terminate an observed activation", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Still observed working"),
  ]);
  state.mainHeader.ownExecution = {
    reference: "activation",
    state: { kind: "observed_active", lastObservedAtMs: 100 },
    durations: [],
  };
  await openTrace(page);
  await expect
    .poll(() => state.reads.filter((read) => read.includes("/changes?")).length)
    .toBeGreaterThan(1);
  await expect(page.getByText(/Current history · Working/)).toBeVisible();
  await expect(page.getByText(/Current history · Completed/)).toHaveCount(0);
});

test("an explicit reset reloads around the retained object without resending a command", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Retained before reset"),
  ]);
  await openTrace(page);
  await page.getByRole("button", { name: "Evidence and details" }).focus();
  await page.route("**/api/v1/trace/changes?*", (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ kind: "reset_required", reason: "Replay checkpoint expired" })}\n\n`,
    }),
  );
  await expect(
    page.getByText("Replay checkpoint expired", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Refresh around this item" }).click();
  await expect
    .poll(() =>
      state.reads.some((read) =>
        read.includes("around=opaque-main-agent-anchor-0"),
      ),
    )
    .toBe(true);
  expect(state.commands).toHaveLength(0);
});
