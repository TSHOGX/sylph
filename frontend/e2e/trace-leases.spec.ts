import { expect, finishDocument, test, type Page } from "./fixtures/test";
import { createSession } from "./fixtures/session";
import type { ReaderBootstrap } from "../lib/trace-types";

function nextBootstrap(page: Page) {
  return page.waitForResponse((response) =>
    /^\/api\/v1\/trace\/agents\/[^/]+$/.test(new URL(response.url()).pathname) &&
    response.status() === 200,
  );
}

async function expectReleased(page: Page, url: string, view: ReaderBootstrap["view"]) {
  const retained = new URL(url);
  retained.searchParams.set("at_snapshot", view.token);
  const response = await page.request.get(retained.href);
  expect(response.status(), await response.text()).toBe(410);
  expect((await response.json()).error.code).toBe("snapshot_expired");
  // Cancellation, rather than the passage of its advertised lifetime, must be
  // what made this formerly readable snapshot unavailable.
  expect(Date.now()).toBeLessThan(view.expiresAtMs);
}

test("document navigation releases its actual server view", async ({ page }) => {
  const bootstrap = nextBootstrap(page);
  await createSession(page);
  const response = await bootstrap;
  const { view } = await response.json() as ReaderBootstrap;
  const retained = new URL(response.url());
  retained.searchParams.set("at_snapshot", view.token);
  const before = await page.request.get(retained.href);
  expect(before.status(), await before.text()).toBe(200);

  await finishDocument(page);
  await expect.poll(
    () => page.request.get(retained.href).then(async (check) =>
      check.status() === 410 ? (await check.json()).error.code : check.status()),
    { timeout: 10_000 },
  ).toBe("snapshot_expired");
  expect(Date.now()).toBeLessThan(view.expiresAtMs);
});

test("repeated graceful document lifetimes keep server views available", async ({ page }) => {
  test.setTimeout(120_000);
  const first = nextBootstrap(page);
  await createSession(page);
  const readerUrl = page.url();
  let response = await first;
  const seen = new Set<string>();

  for (let lifetime = 0; lifetime < 80; lifetime++) {
    if (lifetime > 0) {
      const next = nextBootstrap(page);
      await page.goto(readerUrl);
      response = await next;
    }
    const { view } = await response.json() as ReaderBootstrap;
    expect(seen.has(view.token)).toBe(false);
    seen.add(view.token);
    await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
    await finishDocument(page);
    await expectReleased(page, response.url(), view);
  }
});
