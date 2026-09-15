import { expect, test } from "@playwright/test";
import { runExecution, openExecutionGroups } from "./fixtures/execution";

test("completed work exposes its answer and bounded tool disclosures", async ({
  page,
}) => {
  await runExecution(page, "exercise tools please");
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  const summary = page.locator(".reading-work > button").first();
  await expect(summary).toBeVisible();
  await summary.click();
  if ((await summary.getAttribute("aria-expanded")) === "false")
    await summary.click();
  const group = page
    .locator("[data-node-id] > button[aria-expanded]")
    .filter({ hasText: /file reads/ })
    .first();
  await expect(group).toBeVisible();
  await group.click();
  await expect(page.locator("[data-tool-window]").first()).toBeVisible();
  const bounds = await page.locator("[data-tool-window]").first().boundingBox();
  expect(bounds!.height).toBeLessThanOrEqual(420);
  await expect(
    page
      .locator(".reading-bubble")
      .filter({ hasText: "exercise tools please" }),
  ).toContainText("exercise tools please");
});

test("delegation opens beside the conversation and returns to the full catalogue", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "delegate this please",
  );
  await openExecutionGroups(page, sessionId, turnId);
  const entry = page
    .getByRole("region", { name: "Explore delegation" })
    .getByRole("button")
    .first();
  await expect(entry).toBeVisible();
  await entry.click();
  const panel = page.getByLabel("Work panel", { exact: true }).first();
  await expect(
    panel.getByRole("tab", { name: "Subagents", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  await panel.getByRole("button", { name: "All subagents" }).click();
  await expect(
    panel.getByRole("button", { name: "Verify Failed", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Fanning out to two agents.")).toBeVisible();
});

test("hundreds of calls stay bounded and the catalogue remains complete", async ({
  page,
}) => {
  const mounted: number[] = [];
  for (const count of [50, 500]) {
    const { sessionId, turnId } = await runExecution(
      page,
      `reading benchmark ${count}`,
    );
    const response = await page.request.get(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/conversation`,
    );
    const outline = await response.json();
    const work = outline.nodes.find(
      (node: { turnId: string; event: { kind: string } }) =>
        node.turnId === turnId && node.event.kind === "work",
    );
    expect(work.reads).toBe(count);
    const summary = page.locator(
      `[data-node-id=${JSON.stringify(work.id)}] > button`,
    );
    await summary.click();
    if ((await summary.getAttribute("aria-expanded")) === "false")
      await summary.click();
    const group = page.getByRole("button", {
      name: `${count} file reads`,
      exact: true,
    });
    await group.click();
    const window = page
      .locator("[data-tool-window]")
      .filter({ hasText: `module_${count - 1}` });
    await expect(window).toBeVisible();
    expect((await window.boundingBox())!.height).toBeLessThanOrEqual(420);
    mounted.push(await window.locator("[data-node-id]").count());
    expect(mounted.at(-1)).toBeLessThan(30);
    const showPanel = page.getByRole("button", {
      name: "Show work panel",
    });
    if (await showPanel.isVisible()) await showPanel.click();
    await page.getByRole("button", { name: "Subagents", exact: true }).click();
    const panel = page.getByRole("complementary", { name: "Work panel" });
    await expect(
      panel.getByRole("heading", { name: "Finished · 25" }),
    ).toBeVisible();
    await panel.getByRole("searchbox").fill("Audit 24");
    await panel
      .getByRole("button", { name: "Audit 24 Completed", exact: true })
      .click();
    await expect(
      panel.getByText("The delegated inspection is complete.", { exact: true }),
    ).toBeVisible();
    await panel.getByRole("button", { name: "All subagents" }).click();
    await expect(panel.getByRole("searchbox")).toHaveValue("Audit 24");
  }
  expect(Math.abs(mounted[0] - mounted[1])).toBeLessThanOrEqual(3);
  console.log(
    `tool_window_measurement ${JSON.stringify({ sizes: [50, 500], mounted })}`,
  );
});

test("reading layouts fit desktop and narrow viewports in both themes", async ({
  page,
}, info) => {
  await runExecution(page, "reading benchmark 50");
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
  ).toHaveCount(0);
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const width of [1440, 1280, 1024, 768, 390]) {
      await page.setViewportSize({ width, height: 900 });
      const work = page.locator(".reading-work > button").first();
      if ((await work.getAttribute("aria-expanded")) === "true")
        await work.click();
      await page.locator("main [data-scroller]").evaluate((element) => {
        element.scrollTop = 0;
      });
      await expect(
        page.getByText(
          "Inspection complete. The workspace is ready for review.",
          { exact: true },
        ),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth > window.innerWidth,
        ),
      ).toBe(false);
      const path = info.outputPath(`reading-${width}-${colorScheme}.png`);
      await page.screenshot({ path });
      await info.attach(`reading-${width}-${colorScheme}`, {
        path,
        contentType: "image/png",
      });
    }
  }
});

test("work-panel navigation preserves the draft and supports keyboard tabs and maximize", async ({
  page,
}, info) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const { sessionId, turnId } = await runExecution(
    page,
    "delegate this please",
  );
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
  ).toHaveCount(0);
  const { openExecutionGroups } = await import("./fixtures/execution");
  await openExecutionGroups(page, sessionId, turnId);
  await page
    .getByLabel("Message", { exact: true })
    .fill("Keep this unsent draft");
  await page
    .getByRole("region", { name: "Explore delegation" })
    .getByRole("button")
    .first()
    .click();
  let panel = page.getByRole("complementary", { name: "Work panel" });
  await expect(
    panel.getByRole("button", { name: "All subagents" }),
  ).toBeVisible();
  await page.screenshot({ path: info.outputPath("subagent-detail.png") });
  await panel.getByRole("button", { name: "All subagents" }).click();
  await expect(
    panel.getByRole("button", { name: "Verify Failed", exact: true }),
  ).toBeVisible();
  await page.screenshot({ path: info.outputPath("subagent-directory.png") });
  const subagents = panel.getByRole("tab", { name: "Subagents", exact: true });
  await subagents.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(subagents).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(
    panel.getByRole("tab", { name: "Files", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  await subagents.click();
  await panel.getByRole("button", { name: "Maximize panel" }).click();
  const full = page.getByRole("dialog", { name: "Work panel" });
  await expect(full).toBeVisible();
  await full.getByRole("button", { name: "Restore panel" }).click();
  panel = page.getByRole("complementary", { name: "Work panel" });
  await expect(panel.getByRole("searchbox")).toBeVisible();
  await page.setViewportSize({ width: 1024, height: 900 });
  const narrow = page.getByRole("dialog", { name: "Work panel" });
  await expect(narrow).toBeVisible();
  const bounds = (await narrow.boundingBox())!;
  expect(bounds.x).toBeGreaterThan(0);
  await page.mouse.click(bounds.x / 2, 120);
  await expect(narrow).toBeHidden();
  await expect(page.getByLabel("Message", { exact: true })).toHaveValue(
    "Keep this unsent draft",
  );
});

test("full tool details follow arguments that arrive after inspection opens", async ({
  page,
}) => {
  const { sessionId, turnId } = await runExecution(
    page,
    "exercise tools please",
  );
  await openExecutionGroups(page, sessionId, turnId);
  const base = `/api/v1/sessions/${encodeURIComponent(sessionId)}`;
  const roots = (await (await page.request.get(`${base}/nodes`)).json()).nodes;
  const group = roots.find(
    (node: { turnId: string; reads: number }) =>
      node.turnId === turnId && node.reads > 0,
  );
  const calls = (
    await (
      await page.request.get(
        `${base}/nodes?parent=${encodeURIComponent(group.id)}`,
      )
    ).json()
  ).nodes;
  const call = calls.find(
    (node: { event: { kind: string; name?: string } }) =>
      node.event.kind === "tool_call" && node.event.name === "Read",
  );
  let arrived = false;
  await page.route(
    (url) =>
      url.pathname.endsWith("/nodes") &&
      url.searchParams.get("parent") === call.id,
    async (route) => {
      const response = await route.fetch();
      const payload = await response.json();
      const parent = payload.parentNode;
      parent.revision += arrived ? 1001 : 1000;
      parent.bodyRevision = parent.revision;
      payload.snapshot = Math.max(payload.snapshot, parent.revision);
      parent.event = {
        ...parent.event,
        bodyRef: undefined,
        data: undefined,
        text: arrived ? "Arguments arrived while this panel was open." : "",
      };
      await route.fulfill({ json: payload });
    },
  );
  await page.getByRole("button", { name: /Read · src\/lib.rs/ }).click();
  await page
    .getByRole("button", { name: "Open full details", exact: true })
    .click();
  const panel = page.getByRole("complementary", { name: "Work panel" });
  await expect(
    panel.getByText("No tool input has been recorded yet.", { exact: true }),
  ).toBeVisible();
  arrived = true;
  await expect(panel).toContainText(
    "Arguments arrived while this panel was open.",
  );
});
