import { expect, type Page } from "@playwright/test";
import { createSession } from "./session";
import type { ExecutionNode } from "../../lib/execution-store";

export async function runExecution(page: Page, prompt: string) {
  const sessionId = await createSession(page);
  await page.getByLabel("Message", { exact: true }).fill(prompt);
  const response = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname.endsWith("/turns"),
  );
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const turnId = (await (await response).json()).turn.id as string;
  await expect
    .poll(
      async () => {
        const response = await page.request.get(
          `/api/v1/turns/${encodeURIComponent(turnId)}`,
        );
        return (await response.json()).turn.status;
      },
      { timeout: 30_000 },
    )
    .toMatch(/completed|failed|cancelled|interrupted/);
  return { sessionId, turnId };
}
export async function rootNodes(
  page: Page,
  sessionId: string,
): Promise<ExecutionNode[]> {
  const response = await page.request.get(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/nodes`,
  );
  expect(response.ok()).toBe(true);
  return (await response.json()).nodes;
}
export async function openExecutionGroups(
  page: Page,
  sessionId: string,
  turnId: string,
  touch = false,
) {
  const outline = await page.request.get(`/api/v1/sessions/${encodeURIComponent(sessionId)}/conversation`);
  const work = (await outline.json()).nodes.find((node: ExecutionNode) => node.turnId === turnId && node.event.kind === "work");
  if (work) {
    const summary = page.locator(`[data-node-id=${JSON.stringify(work.id)}] > button[aria-expanded]`);
    await expect(summary).toBeVisible();
    // Establish explicit reading intent even if live work auto-opened before its
    // terminal patch arrived. Then completion cannot race the test's navigation.
    await summary.click();
    if (await summary.getAttribute("aria-expanded") === "false") await summary.click();
  }
  const groups = (await rootNodes(page, sessionId)).filter(
    (node) => node.turnId === turnId && node.event.kind === "execution",
  );
  for (const group of groups) {
    const button = page.locator(
      `[data-node-id=${JSON.stringify(group.id)}] > button[aria-expanded]`,
    );
    await expect(button).toBeVisible();
    if ((await button.getAttribute("aria-expanded")) === "false")
      if (touch) await button.tap();
      else await button.click();
  }
}
