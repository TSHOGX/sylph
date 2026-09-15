import { expect, test } from "@playwright/test";
import {
  openExecutionGroups,
  rootNodes,
  runExecution,
} from "./fixtures/execution";

test("nested failures remain visible before any delegation is loaded", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "delegate this please",
  );
  const roots = (await rootNodes(page, sessionId)).filter(
    (node) => node.turnId === turnId,
  );
  const failed = roots.find(
    (node) => node.event.kind === "execution" && node.failed > 0,
  )!;
  expect(failed).toBeTruthy();
  expect(failed.failed).toBeGreaterThan(0);
  await expect(page.locator(".reading-work").filter({hasText:"failed"})).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Verify delegation" }),
  ).toHaveCount(0);
  await openExecutionGroups(page, sessionId, turnId);
  const verify = page.getByRole("region", { name: "Verify delegation" });
  await expect(verify).toContainText("failed");
  await expect(verify.getByRole("button").first()).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Stranded delegation" }),
  ).toContainText("never finished");
});

test("delegation identities and summaries survive archiving and reload", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "delegate this please",
  );
  await openExecutionGroups(page, sessionId, turnId);
  const verify = page.getByRole("region", { name: "Verify delegation" });
  await expect(verify).toBeVisible();
  const id = await verify.getAttribute("data-node-id");
  await page.reload();
  await openExecutionGroups(page, sessionId, turnId);
  await expect(
    page.getByRole("region", { name: "Verify delegation" }),
  ).toHaveAttribute("data-node-id", id!);
  await expect(
    page.getByRole("region", { name: "Verify delegation" }),
  ).toContainText("failed");
});

test("focus reads only one branch and offers the route back", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "delegate this please",
  );
  await openExecutionGroups(page, sessionId, turnId);
  const explore = page.getByRole("region", { name: "Explore delegation" });
  await explore.getByRole("button").first().click();
  const panel = page.getByRole("complementary", { name: "Work panel" });
  await expect(panel.getByRole("button", { name: "All subagents" })).toBeVisible();
  await expect(panel.getByRole("region", { name: "Verify delegation" })).toHaveCount(0);
  await panel.getByRole("button", { name: "All subagents" }).click();
  await expect(panel.getByRole("button", {name:"Verify Failed",exact:true})).toBeVisible();
  await expect(page.getByText("Fanning out to two agents.")).toBeVisible();
});

test("a promoted host keeps its report separate from its call body", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(page, "promote this please");
  await openExecutionGroups(page, sessionId, turnId);
  const agent = page.getByRole("region", {
    name: "Agent delegation",
    exact: true,
  });
  await expect(agent).toContainText("reported nothing");
  await expect(agent).not.toContainText("summarize");
  await agent.getByRole("button").first().click();
  await expect(page.getByRole("complementary",{name:"Work panel"})).toContainText("summarize");
});
