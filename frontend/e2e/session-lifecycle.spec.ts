import { expect, test } from "@playwright/test";

/**
 * The critical path: create a session, send a turn, watch it stream, and see it
 * settle. This is the flow the README promises a new user, so a break here
 * means a broken first run.
 *
 * Auth needs no setup: the app bootstraps its own cookie from
 * `GET /api/v1/auth/token` on mount.
 */
test("a new session accepts a turn and streams the reply", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();

  await page.getByRole("link", { name: /new session/i }).click();
  await expect(page.getByRole("heading", { name: /new session/i })).toBeVisible();

  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");

  await page.getByRole("button", { name: "Create session" }).click();

  // Creation navigates to the session, so the composer is the signal it worked.
  const composer = page.getByLabel("Message");
  await expect(composer).toBeVisible();

  await composer.fill("Hello from Playwright");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // The mock runner streams a thinking block, then deltas, then a completed
  // reply. Asserting the final text covers the whole delta-accumulation path.
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  await expect(page.getByText("Considering the request.")).toBeVisible();

  // The user's own message is journalled too, so history is complete.
  await expect(page.getByText("Hello from Playwright")).toBeVisible();
});

test("a created session appears in the list with its origin", async ({ page }) => {
  await page.goto("/sessions/new");
  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");
  await page.getByRole("button", { name: "Create session" }).click();
  await expect(page.getByLabel("Message")).toBeVisible();

  await page.goto("/");

  // Sessions created in the WebApp are Sylph-origin. Origin is durable product
  // data, so it must be visible rather than inferred.
  const rows = page.getByRole("listitem");
  await expect(rows.first()).toContainText(/e2e/);
  await expect(rows.first()).toContainText(/sylph/i);
});

test("reloading mid-conversation replays the journal", async ({ page }) => {
  await page.goto("/sessions/new");
  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");
  await page.getByRole("button", { name: "Create session" }).click();

  const composer = page.getByLabel("Message");
  await composer.fill("Persist this");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // A reload drops the SSE connection entirely, so what survives is what the
  // journal holds. Ephemeral deltas are expected to be gone; the completed
  // reply is not.
  await page.reload();

  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  await expect(page.getByText("Persist this")).toBeVisible();
});
