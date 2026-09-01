import { test, expect } from "@playwright/test";

test("draft remains usable while opening files and changing config", async ({ page }) => {
  const bootstrap = await page.request.get("/api/v1/auth/token");
  expect(bootstrap.ok()).toBe(true);
  const env = await page.request.get("/api/v1/environments");
  const envBody = await env.json();
  const environment = envBody.environments[0].id;
  const created = await page.request.post("/api/v1/sessions", {
    data: { environment, agent: "e2e", workdir: "/tmp" },
  });
  expect(created.ok()).toBe(true);
  const { draftId } = await created.json();
  await page.goto(`/sessions/draft/${encodeURIComponent(draftId)}`);
  await expect(page.getByLabel("Message")).toBeVisible();
  await page.getByRole("button", { name: "Show artifacts sidebar" }).click();
  await expect(page.getByRole("heading", { name: "Files" })).toBeVisible();
  await expect(page.getByText("before-send.txt")).toHaveCount(0);
  const tree = await page.request.get(`/api/v1/sessions/draft/${encodeURIComponent(draftId)}/files`);
  expect(tree.ok()).toBe(true);
  const model = page.getByRole("button", { name: /model/i }).first();
  await expect(model).toBeVisible();
  await model.click();
  await expect(page.getByRole("menu")).toBeVisible();
});

test("switching drafts does not discard the replacement", async ({ page }) => {
  const bootstrap = await page.request.get("/api/v1/auth/token");
  expect(bootstrap.ok()).toBe(true);
  const env = await page.request.get("/api/v1/environments");
  const environment = (await env.json()).environments[0].id;
  const created = await page.request.post("/api/v1/sessions", {
    data: { environment, agent: "e2e", workdir: "/tmp" },
  });
  const { draftId } = await created.json();
  await page.goto(`/sessions/draft/${encodeURIComponent(draftId)}`);
  await expect(page.getByLabel("Message")).toBeVisible();
  const menu = page.getByRole("button", { name: "Add" });
  await menu.click();
  await expect(page.getByRole("menu")).toBeVisible();
  const replacement = await page.request.post("/api/v1/sessions", {
    data: { environment, agent: "e2e", workdir: "/tmp" },
  });
  expect(replacement.ok()).toBe(true);
  const replacementId = (await replacement.json()).draftId;
  await page.goto(`/sessions/draft/${encodeURIComponent(replacementId)}`);
  await expect(page.getByLabel("Message")).toBeVisible();
  const metadata = await page.request.get(`/api/v1/sessions/draft/${encodeURIComponent(replacementId)}`);
  expect(metadata.ok()).toBe(true);
});

test("navigating from one empty draft discards the draft left behind", async ({ page }) => {
  const bootstrap = await page.request.get("/api/v1/auth/token");
  expect(bootstrap.ok()).toBe(true);
  const env = await page.request.get("/api/v1/environments");
  const environment = (await env.json()).environments[0].id;
  const first = await page.request.post("/api/v1/sessions", {
    data: { environment, agent: "e2e", workdir: "/tmp" },
  });
  const firstDraft = (await first.json()).draftId;
  await page.goto(`/sessions/draft/${encodeURIComponent(firstDraft)}`);
  await expect(page.getByLabel("Message")).toBeVisible();

  // Use the in-app action rather than a full document navigation. This is the
  // user path where the old draft must be discarded by the route cleanup while
  // the replacement draft remains alive.
  await page.getByRole("button", { name: /Quick Sessions actions/ }).click();
  await page.getByRole("menu").getByRole("menuitem", { name: "New session here", exact: true }).click();
  await expect(page.getByLabel("Message")).toBeVisible();
  const secondDraft = decodeURIComponent(page.url().split("/").pop()!);

  expect(
    (await page.request.get(`/api/v1/sessions/draft/${encodeURIComponent(firstDraft)}`)).status(),
  ).toBe(404);
  expect(
    (await page.request.get(`/api/v1/sessions/draft/${encodeURIComponent(secondDraft)}`)).ok(),
  ).toBe(true);
});

test("starting another session while a draft is open leaves the new draft alive", async ({ page }) => {
  const bootstrap = await page.request.get("/api/v1/auth/token");
  expect(bootstrap.ok()).toBe(true);
  const env = await page.request.get("/api/v1/environments");
  const environment = (await env.json()).environments[0].id;
  const first = await page.request.post("/api/v1/sessions", {
    data: { environment, agent: "e2e", workdir: "/tmp" },
  });
  const firstDraft = (await first.json()).draftId;
  const firstTurn = await page.request.post(`/api/v1/sessions/draft/${encodeURIComponent(firstDraft)}/turns`, {
    data: { text: "seed" },
  });
  const firstSession = (await firstTurn.json()).session;
  await page.goto(`/sessions/${encodeURIComponent(firstSession.id)}`);
  await expect(page.getByLabel("Message")).toBeVisible();
  const group = page.locator("button[aria-expanded]").filter({ hasText: "tmp" }).first();
  await group.click({ button: "right" });
  const menu = page.getByRole("menu");
  await menu.getByRole("menuitem", { name: "New session here", exact: true }).click();
  await expect(page).toHaveURL(/\/sessions\/draft\/draft-/);
  await expect(page.getByLabel("Message")).toBeVisible();
  const secondDraft = decodeURIComponent(page.url().split("/").pop()!);
  const metadata = await page.request.get(`/api/v1/sessions/draft/${encodeURIComponent(secondDraft)}`);
  expect(metadata.ok()).toBe(true);
  const oldMetadata = await page.request.get(`/api/v1/sessions/draft/${encodeURIComponent(firstDraft)}`);
  expect(oldMetadata.status()).toBe(404);
});
