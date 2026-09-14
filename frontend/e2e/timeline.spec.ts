import { expect, test } from "@playwright/test";
import {
  openExecutionGroups,
  rootNodes,
  runExecution,
} from "./fixtures/execution";

test("tools expose summaries first and mount diffs and results only after expansion", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "exercise tools please",
  );
  await expect(
    page.getByRole("region", { name: "Edit tool call" }),
  ).toHaveCount(0);
  await openExecutionGroups(page, sessionId, turnId);
  const edit = page.getByRole("region", { name: "Edit tool call" });
  await expect(edit).toContainText("src/lib.rs");
  await expect(edit).not.toContainText('println!("before");');
  await edit.locator("button[aria-expanded]").click();
  await expect(edit).toContainText('println!("before");');
  await expect(edit).toContainText('tracing::info!("after");');
  const bash = page.getByRole("region", { name: "Bash tool call" });
  await expect(bash).toContainText("failed");
  await bash.locator("button[aria-expanded]").click();
  await expect(page.getByText(/cannot find value `after`/)).toBeVisible();
});

test("outward Markdown replies split consecutive execution groups", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "exercise tools please",
  );
  await expect(page.locator("strong", { hasText: "real" })).toBeVisible();
  const nodes = (await rootNodes(page, sessionId)).filter(
    (node) => node.turnId === turnId,
  );
  const middle = nodes.findIndex(
    (node) =>
      node.event.kind === "assistant" && node.event.text?.includes("logging"),
  );
  expect(nodes[middle - 1].event.kind).toBe("execution");
  expect(nodes[middle + 1].event.kind).toBe("execution");
});

test("a thousand-event reload keeps response and mounted rows bounded", async ({
  page,
}) => {
  test.slow();
  const { sessionId } = await runExecution(page, "bulk render please");
  const exported = await page.request.get(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`,
  );
  const events = (await exported.json()).turns.flatMap(
    (entry: { events: { kind: string }[] }) => entry.events,
  );
  expect(
    events.filter((event: { kind: string }) => event.kind === "tool_call")
      .length,
  ).toBeGreaterThanOrEqual(250);
  const started = Date.now();
  await page.reload();
  await expect(
    page.getByText("Hello from the E2E runner.").last(),
  ).toBeVisible();
  const elapsed = Date.now() - started;
  expect(elapsed).toBeLessThan(2000);
  expect(await page.locator("[data-node-id]").count()).toBeLessThanOrEqual(40);
  const response = await page.request.get(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/nodes`,
  );
  expect((await response.body()).byteLength).toBeLessThanOrEqual(128 * 1024);
  console.log(
    JSON.stringify({
      measurement: "timeline_reload",
      elapsedMs: elapsed,
      mounted: await page.locator("[data-node-id]").count(),
    }),
  );
});

test("disclosure supports keyboard focus and reduced motion without shifting text", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const { sessionId, turnId } = await runExecution(
    page,
    "exercise tools please",
  );
  const group = (await rootNodes(page, sessionId)).find(
    (node) => node.turnId === turnId && node.event.kind === "execution",
  )!;
  const button = page.locator(
    `[data-node-id=${JSON.stringify(group.id)}] > button[aria-expanded]`,
  );
  const label = button.locator("span").first();
  const before = await label.boundingBox();
  await button.focus();
  await page.keyboard.press("Enter");
  await expect(button).toHaveAttribute("aria-expanded", "true");
  expect((await label.boundingBox())!.x).toBe(before!.x);
  await page.keyboard.press("Enter");
  await expect(button).toHaveAttribute("aria-expanded", "false");
});
