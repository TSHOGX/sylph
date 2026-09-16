import { expect, test } from "./fixtures/test";
import { gotoReady } from "./fixtures/session";

/**
 * The browser stack must use a separately paired envagent.
 *
 * The development server may expose a local placeholder while the envagent is
 * still dialing in. Selecting the first row would make the suite pass through a
 * server-side adapter seam, so this asserts the connected inventory and its
 * per-environment fixture agent explicitly.
 */
test("reports the paired envagent inventory", async ({ page }) => {
  await gotoReady(page);

  const response = await page.request.get("/api/v1/environments");
  expect(response.ok()).toBe(true);
  const { environments } = (await response.json()) as {
    environments: {
      id: string;
      origin: string;
      connected: boolean;
      agents: { name: string; available: boolean }[];
    }[];
  };

  expect(
    environments.some(
      (environment) =>
        environment.origin === "self-installed" &&
        environment.connected &&
        environment.agents.some((agent) => agent.name === "codex" && agent.available),
    ),
    "the browser must observe the separately paired envagent and its inventory",
  ).toBe(true);
});
