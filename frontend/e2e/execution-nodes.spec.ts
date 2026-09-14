import { expect, test } from "@playwright/test";
import { createSession } from "./fixtures/session";

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
