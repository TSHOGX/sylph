import { expect, test } from "@playwright/test";
import { createSession } from "./fixtures/session";

test("collapsed execution shows live activity before the final reply exists", async ({
  page,
}) => {
  await createSession(page);
  await page.getByLabel("Message", { exact: true }).fill("paced please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const group = page
    .locator("[data-node-id] > button[aria-expanded=false]")
    .filter({ hasText: /Thinking/ })
    .last();
  await expect(group).toBeVisible({ timeout: 8000 });
  await expect(group.locator("[data-running-activity]")).toBeVisible();
  await expect(page.getByText("Reasoning step one.")).toHaveCount(0);
  await expect(page.getByText("Hello from the E2E runner.")).toBeHidden();
  await group.click();
  const thinking = page
    .locator("[data-node-id] > button[aria-expanded=false]")
    .filter({ hasText: /^Thinking/ })
    .last();
  await thinking.click();
  await expect(page.getByText(/Reasoning step one\./)).toBeVisible();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible({
    timeout: 10000,
  });
  await expect(
    page.locator("[data-node-id] [data-running-activity]"),
  ).toHaveCount(0);
});

test("active summaries name the running file without loading the call body", async ({
  page,
}) => {
  await createSession(page);
  await page.getByLabel("Message", { exact: true }).fill("paced please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(
    page
      .locator("[data-node-id] > button")
      .filter({ hasText: /src\/paced.rs/ }),
  ).toBeVisible({ timeout: 8000 });
  await expect(
    page.getByRole("region", { name: "Read tool call" }),
  ).toHaveCount(0);
  await expect(page.getByText("Hello from the E2E runner.")).toBeHidden();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible({
    timeout: 10000,
  });
});
