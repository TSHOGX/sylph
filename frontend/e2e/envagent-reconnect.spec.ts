import fs from "node:fs";
import path from "node:path";
import { expect, test } from "@playwright/test";
import { gotoReady } from "./fixtures/session";

/**
 * A real envagent process restart must leave the paired environment usable.
 *
 * The wrapper watches this marker only in E2E; production has no filesystem
 * control surface. Waiting for its receipt proves the process was replaced,
 * then the API assertion proves the bridge accepted the reconnecting hello.
 */
test("reconnects the paired environment after an envagent restart", async ({ page }) => {
  await gotoReady(page);

  const root = path.resolve(process.cwd(), ".e2e-env-home");
  const marker = path.join(root, "restart-agent");
  const receipt = path.join(root, "restart-complete");
  fs.rmSync(receipt, { force: true });
  fs.writeFileSync(marker, "restart\n");

  await expect.poll(() => fs.existsSync(receipt), { timeout: 30_000 }).toBe(true);

  const response = await page.request.get("/api/v1/environments");
  expect(response.ok()).toBe(true);
  const { environments } = (await response.json()) as {
    environments: { connected: boolean; agents: { name: string; available: boolean }[] }[];
  };
  expect(
    environments.some(
      (environment) =>
        environment.connected &&
        environment.agents.some((agent) => agent.name === "codex" && agent.available),
    ),
  ).toBe(true);
});
