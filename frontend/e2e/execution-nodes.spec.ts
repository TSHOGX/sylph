import { expect, test, type Page } from "@playwright/test";
import { createSession } from "./fixtures/session";
import type { ExecutionNode } from "../lib/execution-store";

async function pagedHistory(page: Page) {
  const sessionId = await createSession(page);
  const state = { latest: 399, patchReads: 0 };
  const node = (id: number): ExecutionNode => ({
    id: `reply-${id}`, parentId: "", turnId: "history-turn", ordinal: id, orderTie: id,
    revision: id + 1, bodyRevision: id + 1, children: 0, tools: 0, reads: 0, commands: 0,
    thinking: 0, failed: 0, running: 0, waiting: 0, activity: "",
    event: { seq: id, kind: "assistant", text: `History reply ${id}`, stability: "stable",
      source: "agent", arrivalAtMs: id, createdAtMs: id },
  });
  await page.route(`**/api/v1/sessions/${encodeURIComponent(sessionId)}/nodes?*`, (route) => {
    const params = new URL(route.request().url()).searchParams;
    const changes = params.has("changes");
    if (changes) state.patchReads++;
    const latest = state.latest;
    const boundary = Number(params.get("cursor")?.split(":")[0] ?? latest + 1);
    const end = Math.min(latest + 1, boundary);
    const start = Math.max(0, end - 40);
    return route.fulfill({ json: {
      parent: "", snapshot: latest + 1,
      nodes: changes ? (latest === 400 ? [node(400)] : []) : Array.from({ length: end - start }, (_, i) => node(start + i)),
      hasMore: !changes && start > 0,
      nextCursor: changes ? `${latest + 2}:` : start > 0 ? `${start}:${start}:reply-${start}` : null,
    } });
  });
  await page.reload();
  await expect(page.getByText("History reply 399", { exact: true })).toBeVisible();
  return state;
}

test("prepending history preserves the mounted reading position", async ({ page }) => {
  await pagedHistory(page);
  const scroller = page.locator("[data-scroller]");
  await scroller.evaluate((element) => { element.scrollTop = 0; });
  const anchor = page.locator('[data-node-id="reply-360"]');
  await expect(anchor).toBeVisible();
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
  });
  const before = (await anchor.boundingBox())!.y;
  const geometry = () => anchor.evaluate((element) => {
    const pane = element.closest<HTMLElement>("[data-scroller]")!;
    const row = element.parentElement!;
    const top = row.parentElement!;
    return { y: element.getBoundingClientRect().y, scroll: pane.scrollTop,
      margin: top.getBoundingClientRect().y + pane.scrollTop - pane.getBoundingClientRect().y,
      transform: row.style.transform, paneY: pane.getBoundingClientRect().y };
  });
  const initial = await geometry();
  await page.getByRole("button", { name: "Load earlier history", exact: true }).click();
  await expect(anchor.locator("..")).toHaveAttribute("data-index", "40");
  await page.evaluate(async () => { await new Promise(requestAnimationFrame); await new Promise(requestAnimationFrame); });
  await expect.poll(async () => Math.abs((await anchor.boundingBox())!.y - before)).toBeLessThan(4);
  console.log(`anchor_measurement ${JSON.stringify({ before: initial, after: await geometry() })}`);
  expect(await page.locator("[data-node-id]").count()).toBeLessThan(40);
});

test("a bounded historical window returns explicitly to an advancing live tail", async ({ page }) => {
  const state = await pagedHistory(page);
  const scroller = page.locator("[data-scroller]");
  for (let i = 0; i < 4; i++) {
    await page.getByRole("button", { name: "Load earlier history", exact: true }).click();
    await expect.poll(async () => {
      await scroller.evaluate((element) => { element.scrollTop = 0; });
      return page.getByText(`History reply ${320 - i * 40}`, { exact: true }).isVisible();
    }).toBe(true);
  }
  await expect(page.getByRole("button", { name: "Back to latest", exact: true })).toBeVisible();
  state.latest = 400;
  const reads = state.patchReads;
  await expect.poll(() => state.patchReads).toBeGreaterThan(reads + 1);
  await expect(page.getByText("History reply 400", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Back to latest", exact: true }).click();
  await scroller.evaluate((element) => { element.scrollTop = element.scrollHeight; });
  await expect(page.getByText("History reply 400", { exact: true })).toBeVisible();
});

test("execution is fetched one disclosure level at a time and collapse all never expands", async ({
  page,
}) => {
  const nodeReads: string[] = [],
    bodyReads: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/nodes?")) nodeReads.push(request.url());
    if (request.url().includes("/body") || request.url().includes("/bodies/"))
      bodyReads.push(request.url());
  });
  await createSession(page);
  await page
    .getByLabel("Message", { exact: true })
    .fill("exercise tools please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  expect(
    nodeReads.every((url) => new URL(url).searchParams.get("parent") === ""),
  ).toBe(true);
  expect(bodyReads).toHaveLength(0);
  await expect(
    page.getByRole("button", { name: "Expand all", exact: true }),
  ).toHaveCount(0);
  const group = page
    .locator("button[aria-expanded=false]")
    .filter({ hasText: /file reads/ })
    .first();
  await expect(
    page.locator("[data-node-id] button").filter({ hasText: /failed/ }),
  ).toBeVisible();
  await group.click();
  const read = page.getByRole("button", { name: /Read · src\/lib.rs/ });
  await expect(read).toBeVisible();
  await expect(
    page.getByText('println!("before");', { exact: true }),
  ).toHaveCount(0);
  await read.click();
  await expect(read).toHaveAttribute("aria-expanded", "true");
  await page.getByRole("button", { name: "Collapse all", exact: true }).click();
  await expect(
    page.locator("[data-node-id] button[aria-expanded=true]"),
  ).toHaveCount(0);
  expect(bodyReads).toHaveLength(0);
});

test("reading a large body does not refetch it on subsequent streaming patches", async ({
  page,
}) => {
  const sessionId = await createSession(page);
  let revision = 1,
    bodyReads = 0,
    nodeReads = 0;
  const base = `**/api/v1/sessions/${encodeURIComponent(sessionId)}`;
  await page.route(`${base}/nodes?*`, (route) => {
    nodeReads++;
    return route.fulfill({
      json: {
        parent: "",
        snapshot: revision,
        nextCursor: null,
        hasMore: false,
        nodes: [
          {
            id: "body-node",
            parentId: "",
            turnId: "body-turn",
            ordinal: 1,
            orderTie: 1,
            revision,
            bodyRevision: revision,
            event: {
              seq: revision,
              kind: "assistant",
              phase: "delta",
              text: `Preview ${revision}`,
              stability: "ephemeral",
              source: "agent",
              createdAtMs: 1,
              arrivalAtMs: 1,
              bodyRef: { seq: revision, textBytes: 100_000, dataBytes: 0 },
            },
            children: 0,
            tools: 0,
            reads: 0,
            commands: 0,
            thinking: 0,
            failed: 0,
            running: 0,
            waiting: 0,
            activity: "",
          },
        ],
      },
    });
  });
  await page.route(`${base}/nodes/body-node/body`, (route) => {
    bodyReads++;
    return route.fulfill({
      json: { text: `Full content revision ${revision}` },
    });
  });
  await page.reload();
  await expect(page.getByText("Preview 1", { exact: true })).toBeVisible();
  expect(bodyReads).toBe(0);
  await page
    .getByRole("button", { name: "Load full content", exact: true })
    .click();
  await expect(
    page.getByText("Full content revision 1", { exact: true }),
  ).toBeVisible();
  revision = 2;
  await expect(
    page.getByRole("button", { name: "Load latest content", exact: true }),
  ).toBeVisible();
  const readCount = nodeReads;
  await expect.poll(() => nodeReads).toBeGreaterThan(readCount + 1);
  expect(bodyReads).toBe(1);
  await expect(
    page.getByText("Full content revision 1", { exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Load latest content", exact: true })
    .click();
  await expect(
    page.getByText("Full content revision 2", { exact: true }),
  ).toBeVisible();
  expect(bodyReads).toBe(2);
});
