import { expect, test } from "./fixtures/test";
import { installTraceFixture, message, openTrace } from "./fixtures/trace";

test("partial payloads open retained intervals and skip missing bytes in both directions", async ({ page }) => {
  const row = message(0, "");
  if (row.content.kind !== "message") throw new Error("Expected message fixture");
  const part = row.content.parts[0];
  part.previewTruncated = true;
  part.payload.totalBytes = 512;
  part.payload.availability = { kind: "partial", reason: "Two retained intervals" };
  part.payload.availableRanges = [{ start: 128, endExclusive: 140 }, { start: 256, endExclusive: 268 }];
  await installTraceFixture(page, [row]);
  const requests: number[][] = [];
  await page.route((url) => url.pathname === "/api/v1/trace/payloads/body-0", async (route) => {
    const url = new URL(route.request().url());
    const start = Number(url.searchParams.get("start"));
    const end = Number(url.searchParams.get("end"));
    requests.push([start, end]);
    const text = start === 128 && end === 140 ? "first range." : start === 256 && end === 268 ? "last range!!" : null;
    if (text === null) return route.fulfill({ status: 416, json: { error: { code: "unavailable_range", message: "These bytes were not retained" } } });
    return route.fulfill({ status: 200, contentType: "text/plain", headers: { "content-range": `bytes ${start}-${end - 1}/512` }, body: text });
  });
  await openTrace(page);
  await page.getByRole("button", { name: "Read recorded conversation result", exact: true }).click();
  await expect(page.getByText("first range.", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Previous range" })).toBeDisabled();
  await page.getByRole("button", { name: "Next range" }).click();
  await expect(page.getByText("last range!!", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Next range" })).toBeDisabled();
  await page.getByRole("button", { name: "Previous range" }).click();
  await expect(page.getByText("first range.", { exact: true })).toBeVisible();
  expect(requests).toEqual([[128, 140], [256, 268], [128, 140]]);
});
