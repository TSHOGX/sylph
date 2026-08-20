import { expect, test } from "@playwright/test";

/**
 * The critical path: create a session, send a turn, watch it stream, and see it
 * settle. This is the flow the README promises a new user, so a break here means
 * a broken first run.
 *
 * Auth needs no setup: the app bootstraps its own cookie from
 * `GET /api/v1/auth/token` on mount.
 *
 * Selectors are scoped to `#main` where the page and the shell would otherwise
 * both match. The left sidebar lists sessions as `listitem`s too, so an unscoped
 * role query finds sidebar chrome rather than page content.
 */
test("a new session accepts a turn and streams the reply", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();

  // "New session" is a top-bar button in the three-pane shell, not a page link.
  await page.getByRole("button", { name: "New session" }).click();
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

  // Thinking is collapsed by default in the agent-first timeline, so its text is
  // in the DOM but hidden until the disclosure is opened. That it is *not*
  // visible is the assertion: reasoning must not compete with output.
  const thinking = page.getByText("Considering the request.");
  await expect(thinking).toBeHidden();
  await page.getByText("Thinking").first().click();
  await expect(thinking).toBeVisible();

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
  const rows = page.locator("#main").getByRole("listitem");
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
  // journal holds. Ephemeral deltas are expected to be gone; the completed reply
  // is not.
  await page.reload();

  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  await expect(page.getByText("Persist this")).toBeVisible();
});

/** The session a run creates must show up under its workspace in the sidebar. */
test("the sidebar groups a new session under its workspace", async ({ page }) => {
  await page.goto("/sessions/new");
  await page.getByLabel("Agent").selectOption("e2e");
  await page.getByLabel("Working directory").fill("/tmp");
  await page.getByRole("button", { name: "Create session" }).click();
  await expect(page.getByLabel("Message")).toBeVisible();

  const sidebar = page.getByRole("complementary", { name: "Workspaces" });
  await expect(sidebar).toBeVisible();

  // Grouping is by workdir, so `/tmp` becomes a workspace named `tmp`.
  const group = sidebar.getByRole("button", { name: /tmp/ });
  await expect(group.first()).toBeVisible();

  // The open session is marked as the current page for assistive technology.
  await expect(sidebar.locator('a[aria-current="page"]')).toBeVisible();
});
