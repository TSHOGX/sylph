import { expect, test } from "./fixtures/test";
import {
  agentLink,
  installTraceFixture,
  MAIN_AGENT,
  message,
  openTrace,
  searchPayload,
  traceLimits,
  tracePage,
  traceView,
} from "./fixtures/trace";

for (const width of [1280, 390]) {
  test(`matched content opens its immutable body window while surrounding history has a later revision at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    const fixture = await installTraceFixture(page, [
      message(0, "Later replacement body"),
    ]);
    const start = traceLimits.maxPayloadBytes + 64;
    fixture.bodies.set(
      "old-body",
      "x".repeat(start) + "Exact older matched body",
    );
    const access = searchPayload(
      fixture.bodies.get("old-body")!,
      "old-body",
      `${MAIN_AGENT}-anchor-0`,
    );
    access.bodyRevision = "older-composed-revision";
    const view = traceView(12);
    const reads: URL[] = [];
    await page.route("**/api/v1/trace/search?*", (route) =>
      route.fulfill({
        json: tracePage(
          [
            {
              anchor: `${MAIN_AGENT}-anchor-0`,
              agent: agentLink(),
              history: {
                ...fixture.mainHeader.history,
                archive: true,
                isCurrent: false,
                label: "Retained archive",
              },
              excerpt: "matched excerpt",
              byteStart: start,
              payload: access,
            },
          ],
          view,
        ),
      }),
    );
    await page.route("**/api/v1/trace/payloads/old-body?*", (route) => {
      reads.push(new URL(route.request().url()));
      return route.fallback();
    });
    await openTrace(page);
    await page
      .getByRole("button", { name: "Find in this agent", exact: true })
      .click();
    await page
      .getByLabel("Find in Main agent", { exact: true })
      .fill("matched");
    await page
      .getByRole("button", { name: "Search retained history", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Read matched content", exact: true })
      .click();
    await expect(
      page.getByText("Exact older matched body", { exact: true }),
    ).toBeVisible();
    await page.screenshot({ path: `/tmp/sylph-matched-content-${width}.png` });
    expect(reads).toHaveLength(1);
    expect(reads[0].searchParams.get("search_result")).toBe("result-old-body");
    expect(reads[0].searchParams.get("revision")).toBe(access.bodyRevision);
    expect(reads[0].searchParams.get("at_snapshot")).toBe(view.token);
    expect(Number(reads[0].searchParams.get("start"))).toBeGreaterThan(0);
    expect(Number(reads[0].searchParams.get("start"))).toBeLessThanOrEqual(
      start,
    );
    expect(
      Number(reads[0].searchParams.get("end")) -
        Number(reads[0].searchParams.get("start")),
    ).toBeLessThanOrEqual(traceLimits.maxPayloadBytes);
    await page
      .getByRole("link", {
        name: "Open Retained archive around this item",
        exact: true,
      })
      .click();
    await expect(
      page.getByText("Later replacement body", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", {
        name: "Return to current conversation to send",
        exact: true,
      }),
    ).toBeVisible();
    expect(fixture.commands).toHaveLength(0);
  });
}

test("a global search can refresh an expired body result without changing its query", async ({
  page,
}) => {
  const fixture = await installTraceFixture(page);
  fixture.bodies.set("expired-body", "Recovered exact matched body");
  let searches = 0;
  let expired = true;
  await page.route("**/api/v1/trace/search?*", (route) => {
    searches++;
    const access = searchPayload(
      "Recovered exact matched body",
      "expired-body",
      `${MAIN_AGENT}-anchor-0`,
    );
    access.authority = {
      kind: "search_result",
      resultRef: `result-${searches}`,
    };
    return route.fulfill({
      json: tracePage(
        [
          {
            anchor: `${MAIN_AGENT}-anchor-0`,
            agent: agentLink(),
            history: {
              ...fixture.mainHeader.history,
              archive: true,
              isCurrent: false,
            },
            excerpt: "matched excerpt",
            byteStart: 0,
            payload: access,
          },
        ],
        traceView(10 + searches),
      ),
    });
  });
  await page.route("**/api/v1/trace/payloads/expired-body?*", (route) => {
    if (expired)
      return route.fulfill({
        status: 410,
        json: {
          error: { code: "snapshot_expired", message: "Search result expired" },
        },
      });
    return route.fallback();
  });
  await openTrace(page);
  await page.keyboard.press("ControlOrMeta+k");
  const dialog = page.getByRole("dialog", {
    name: "Find conversations",
    exact: true,
  });
  await dialog
    .getByRole("textbox", {
      name: "Search retained conversations",
      exact: true,
    })
    .fill("matched");
  await dialog.getByRole("button", { name: "Search", exact: true }).click();
  await dialog
    .getByRole("button", { name: "Read matched content", exact: true })
    .click();
  await expect(dialog.getByRole("alert")).toBeVisible();
  expired = false;
  await dialog.getByRole("button", { name: "Search", exact: true }).click();
  await expect.poll(() => searches).toBe(2);
  await dialog
    .getByRole("button", { name: "Read matched content", exact: true })
    .click();
  await expect(
    dialog.getByText("Recovered exact matched body", { exact: true }),
  ).toBeVisible();
  expect(fixture.commands).toHaveLength(0);
});
