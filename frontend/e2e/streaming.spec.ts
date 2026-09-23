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

test("following latest crosses page boundaries and growing rows without pinning an old anchor", async ({
  page,
}) => {
  const state = await installTraceFixture(
    page,
    Array.from({ length: 45 }, (_, index) => message(index)),
  );
  await openTrace(page);
  await expect(
    page.getByText("Retained message 44", { exact: true }),
  ).toBeVisible();
  for (const end of [95, 145, 195]) {
    state.rows = Array.from({ length: end }, (_, index) =>
      message(
        index,
        index === end - 2
          ? "Growing row\n".repeat(20)
          : `Live message ${index}`,
      ),
    );
    state.revision++;
    await expect(
      page.getByText(`Live message ${end - 1}`, { exact: true }),
    ).toBeVisible();
  }
  const refreshes = state.reads.filter((read) =>
    /^GET \/api\/v1\/trace\/agents\/opaque-main-agent(?:\?|$)/.test(read),
  );
  expect(refreshes.length).toBeGreaterThanOrEqual(4);
  expect(refreshes.every((read) => !read.includes("around="))).toBe(true);
});

test("opening latest history after paused reading resumes later arrivals", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [message(0, "Paused content")]);
  await openTrace(page);
  await page.getByRole("button", { name: "Evidence and details" }).focus();
  state.rows.push(message(1, "First new reply"));
  state.revision++;
  await expect(
    page.getByText("History updated", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("First new reply", { exact: true })).toHaveCount(
    0,
  );
  await page
    .getByRole("button", { name: "Open latest history", exact: true })
    .click();
  await expect(
    page.getByText("First new reply", { exact: true }),
  ).toBeVisible();
  state.rows.push(message(2, "Second new reply"));
  state.revision++;
  await expect(
    page.getByText("Second new reply", { exact: true }),
  ).toBeVisible();
  expect(state.commands).toHaveLength(0);
});
