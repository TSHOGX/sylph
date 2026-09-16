import { expect, test } from "@playwright/test";
import {
  agentLink,
  bootstrap,
  CHILD_AGENT,
  installTraceFixture,
  message,
  openTrace,
  unavailableExecution,
} from "./fixtures/trace";

async function directory(page: import("@playwright/test").Page) {
  const state = await installTraceFixture(page, [
    message(0, "Parent retained answer"),
  ]);
  state.catalogue = Array.from({ length: 1000 }, (_, index) => ({
    agent: agentLink(
      index === 999 ? CHILD_AGENT : `opaque-child-${index}`,
      index === 999 ? "Needle child" : `Agent ${index}`,
    ),
    relation: "spawn",
    ownExecution: unavailableExecution,
    activations: { value: 1, scope: "Agent", complete: true },
    requiredInput: { value: 0, scope: "Agent", complete: true },
    coverage: [],
  }));
  state.mainHeader.ownExecution = {
    reference: "parent-activation",
    state: { kind: "terminated", outcome: "succeeded", reason: null },
    durations: [],
  };
  state.childHeader.ownExecution = {
    reference: "child-activation",
    state: { kind: "observed_active", lastObservedAtMs: 100 },
    durations: [],
  };
  return state;
}

test("the agent directory searches its complete scope without fetching child bodies", async ({
  page,
}) => {
  const state = await directory(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "Agent 0", exact: true }),
  ).toBeVisible();
  expect(
    state.reads.filter(
      (read) =>
        read.includes(`/agents/${CHILD_AGENT}?`) ||
        read.endsWith(`/agents/${CHILD_AGENT}`),
    ),
  ).toHaveLength(0);
  await page.getByLabel("Find agents").fill("Needle");
  await expect(
    page.getByRole("link", { name: "Needle child", exact: true }),
  ).toBeVisible();
  expect(state.searched).toContain("Needle");
});

test("a child uses its own identity and lifecycle in the same companion reader", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await directory(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await page.getByLabel("Find agents").fill("Needle");
  await page
    .getByRole("button", { name: "Open alongside", exact: true })
    .click();
  const inspector = page.getByLabel("Trace inspector", { exact: true });
  await expect(
    inspector.getByRole("heading", { name: /Unnamed agent/ }),
  ).toBeVisible();
  await expect(inspector.getByText(/Current history · Working/)).toBeVisible();
  await expect(page.getByText(/Current history · Completed/)).toBeVisible();
  await expect(
    inspector.getByText("The child owns this answer", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveCount(1);
  await inspector.getByRole("button", { name: "All agents" }).click();
  await expect(page.getByLabel("Find agents")).toBeVisible();
});

test("mobile child inspection fills the viewport and returns without changing control focus", async ({
  page,
}) => {
  await page.setViewportSize({ width: 375, height: 720 });
  await directory(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await page.getByLabel("Find agents").fill("Needle");
  await page
    .getByRole("button", { name: "Open alongside", exact: true })
    .click();
  const inspector = page.getByRole("dialog", { name: "Trace inspector" });
  await expect(inspector).toBeVisible();
  expect((await inspector.boundingBox())!.height).toBeGreaterThan(700);
  await inspector.getByRole("button", { name: "Back to conversation" }).click();
  await expect(inspector).toHaveCount(0);
  await expect(
    page.getByText("Parent retained answer", { exact: true }),
  ).toBeVisible();
});

test("an independently opened embedded agent never gains its anchor session's controls", async ({
  page,
}) => {
  const state = await installTraceFixture(page);
  state.childHeader = bootstrap(CHILD_AGENT).header;
  await openTrace(page, CHILD_AGENT);
  await expect(
    page.getByText("Embedded child has no independent control route", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Send", exact: true }),
  ).toHaveCount(0);
  expect(state.commands).toHaveLength(0);
});
