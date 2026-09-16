import { expect, test } from "./fixtures/test";
import { createSession } from "./fixtures/session";

test("own activations open their retained execution details from History", async ({
  page,
}, testInfo) => {
  await createSession(page);
  await page.getByRole("button", { name: "History", exact: true }).click();
  const catalogue = page.getByRole("region", { name: "Own activations" });
  await expect(catalogue).toBeVisible();
  await expect(catalogue.getByText("Completed", { exact: true })).toBeVisible();
  await testInfo.attach("activation-catalogue-desktop", {
    body: await page.screenshot(),
    contentType: "image/png",
  });
  const detailRequest = page.waitForRequest((request) =>
    new URL(request.url()).pathname.startsWith("/api/v1/trace/objects/"),
  );
  await catalogue.getByRole("button", { name: "Inspect activation" }).click();
  const request = new URL((await detailRequest).url());
  expect(request.searchParams.has("at_snapshot")).toBe(true);
  expect(request.searchParams.has("selection")).toBe(false);
  const inspector = page.getByRole("complementary", {
    name: "Trace inspector",
  });
  await expect(
    inspector.getByRole("heading", { name: "Activation", exact: true }),
  ).toBeVisible();
  await expect(inspector.getByText("Completed", { exact: true })).toBeVisible();
  await testInfo.attach("activation-detail-desktop", {
    body: await page.screenshot(),
    contentType: "image/png",
  });

  await inspector.getByRole("button", { name: "Close", exact: true }).click();
  await page.setViewportSize({ width: 412, height: 915 });
  await page.getByRole("button", { name: "History", exact: true }).click();
  await expect(catalogue.getByText("Completed", { exact: true })).toBeVisible();
  await testInfo.attach("activation-catalogue-mobile-width", {
    body: await page.screenshot(),
    contentType: "image/png",
  });
  await catalogue.getByRole("button", { name: "Inspect activation" }).click();
  const mobileInspector = page.getByRole("dialog", { name: "Trace inspector" });
  await expect(
    mobileInspector.getByRole("heading", { name: "Activation", exact: true }),
  ).toBeVisible();
  await expect(mobileInspector.getByText("Completed", { exact: true })).toBeVisible();
  await testInfo.attach("activation-detail-mobile-width", {
    body: await page.screenshot(),
    contentType: "image/png",
  });
});
