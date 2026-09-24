import { expect, test } from "./fixtures/test";
import {
  installTraceFixture,
  message,
  openTrace,
  MAIN_AGENT,
  CHILD_AGENT,
  agentLink,
  tracePage,
  traceView,
  traceLimits,
} from "./fixtures/trace";

for (const width of [1280, 390]) {
  test(`origin assertions remain paged and distinguish unavailable targets at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const fixture = await installTraceFixture(page, [
      message(0, "Retained fork history"),
    ]);
    fixture.mainHeader.origin = null;
    fixture.mainHeader.history.selection = "child-selection";
    fixture.childHeader.agent = agentLink(CHILD_AGENT, "Original reader");
    fixture.childHeader.parent = null;
    const requests: URL[] = [],
      claims: URL[] = [];
    const values = Array.from({ length: traceLimits.maxItems + 7 }, (_, i) => ({
      claim: `origin-proof-${i}`,
      origin:
        i % traceLimits.maxItems === 0
          ? agentLink(CHILD_AGENT, "Original reader")
          : null,
      availability:
        i % traceLimits.maxItems === 0
          ? { kind: "available" }
          : {
              kind: "unavailable",
              reason:
                i % 2
                  ? "Origin metadata is unavailable"
                  : "The retained origin reference has not been resolved",
            },
    }));
    let failed = false;
    await page.route(
      `**/api/v1/trace/agents/${MAIN_AGENT}/origins?*`,
      async (route) => {
        const url = new URL(route.request().url());
        requests.push(url);
        expect(url.searchParams.get("selection")).toBeNull();
        const start = url.searchParams.has("before")
          ? Math.max(
              0,
              Number(url.searchParams.get("before")) - traceLimits.maxItems,
            )
          : Number(url.searchParams.get("after") ?? 0);
        if (start > 0 && !failed) {
          failed = true;
          return route.fulfill({
            status: 502,
            json: {
              error: {
                code: "READ_FAILED",
                message: "Origin page unavailable",
              },
            },
          });
        }
        return route.fulfill({
          json: tracePage(
            values.slice(start, start + traceLimits.maxItems),
            traceView(),
            start,
            values.length,
          ),
        });
      },
    );
    await page.route(
      `**/api/v1/trace/objects/${MAIN_AGENT}/evidence?*`,
      (route) => route.fulfill({ json: tracePage([], traceView()) }),
    );
    await page.route(
      `**/api/v1/trace/objects/${MAIN_AGENT}/claims/*`,
      (route) => {
        claims.push(new URL(route.request().url()));
        return route.fulfill({
          status: 502,
          json: {
            error: {
              code: "PROOF_READ_FAILED",
              message: "Specific origin proof requested",
            },
          },
        });
      },
    );
    await openTrace(page);
    await page
      .getByLabel("Unsent draft", { exact: true })
      .fill("Keep the fork draft");
    expect(requests).toHaveLength(0);
    await page.getByRole("button", { name: "Origins", exact: true }).click();
    const catalogue = page.getByRole("region", {
      name: "Context origins",
      exact: true,
    });
    await expect(catalogue.getByRole("listitem")).toHaveCount(
      traceLimits.maxItems,
    );
    await expect(
      catalogue.getByText("Origin unavailable", { exact: true }),
    ).toHaveCount(traceLimits.maxItems - 1);
    expect(claims).toHaveLength(0);
    await catalogue
      .getByRole("button", { name: "Inspect origin proof", exact: true })
      .nth(1)
      .click();
    await expect(catalogue.getByRole("alert")).toHaveText(
      "Specific origin proof requested",
    );
    expect(claims[0].pathname).toContain("origin-proof-1");
    expect(claims[0].searchParams.get("at_snapshot")).toBe(traceView().token);
    expect(claims[0].searchParams.get("selection")).toBeNull();
    await catalogue
      .getByRole("button", { name: "Next page", exact: true })
      .click();
    await expect(catalogue.getByRole("alert")).toHaveText(
      "Origin page unavailable",
    );
    const failedUrl = requests.at(-1)!.href;
    await catalogue
      .getByRole("button", { name: "Retry this read", exact: true })
      .click();
    await expect(catalogue.getByRole("listitem")).toHaveCount(7);
    expect(requests.at(-1)!.href).toBe(failedUrl);
    await catalogue
      .getByRole("button", { name: "Previous page", exact: true })
      .click();
    await expect(catalogue.getByRole("listitem")).toHaveCount(
      traceLimits.maxItems,
    );
    await expect(
      catalogue.getByRole("heading", { name: "Context origins", exact: true }),
    ).toBeFocused();
    await testInfo.attach(`origin-catalogue-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
    await catalogue
      .getByRole("button", { name: "Next page", exact: true })
      .click();
    await expect(catalogue.getByRole("listitem")).toHaveCount(7);
    await catalogue
      .getByRole("button", { name: "Origin: Original reader", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "Original reader", exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "Back to previous detail", exact: true })
      .click();
    await expect(catalogue.getByRole("listitem")).toHaveCount(7);
    const inspector = page.getByRole(width < 768 ? "dialog" : "complementary", {
      name: "Trace inspector",
    });
    await inspector
      .getByRole("button", {
        name: width < 768 ? "Back to conversation" : "Close",
        exact: true,
      })
      .click();
    await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
      "Keep the fork draft",
    );
    expect(fixture.commands).toHaveLength(0);
  });
}

test("a companion origin catalogue keeps the companion view and proof scope", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Main retained history"),
  ]);
  state.childHeader.agent = agentLink(CHILD_AGENT, "Origin companion");
  state.childHeader.history.selection = "companion-branch";
  const urls: URL[] = [];
  await page.route("**/api/v1/trace/agents/*/origins?*", (route) => {
    const url = new URL(route.request().url());
    urls.push(url);
    return route.fulfill({
      json: tracePage(
        url.pathname.includes(MAIN_AGENT)
          ? [
              {
                claim: "main-origin",
                origin: agentLink(CHILD_AGENT, "Origin companion"),
                availability: { kind: "available" },
              },
            ]
          : [
              {
                claim: "companion-origin",
                origin: null,
                availability: {
                  kind: "unavailable",
                  reason: "Companion origin unresolved",
                },
              },
            ],
        traceView(),
      ),
    });
  });
  const proofs: URL[] = [];
  await page.route(
    `**/api/v1/trace/objects/${CHILD_AGENT}/evidence?*`,
    (route) => route.fulfill({ json: tracePage([], traceView()) }),
  );
  await page.route(
    `**/api/v1/trace/objects/${CHILD_AGENT}/claims/*`,
    (route) => {
      proofs.push(new URL(route.request().url()));
      return route.fulfill({
        status: 502,
        json: {
          error: { code: "PROOF", message: "Companion proof requested" },
        },
      });
    },
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Origins", exact: true }).click();
  await page
    .getByRole("button", { name: "Origin: Origin companion", exact: true })
    .click();
  const inspector = page.getByRole("complementary", {
    name: "Trace inspector",
  });
  await inspector.getByRole("button", { name: "Origins", exact: true }).click();
  await expect(
    inspector.getByText("Companion origin unresolved", { exact: true }),
  ).toBeVisible();
  expect(urls.at(-1)!.pathname).toContain(CHILD_AGENT);
  expect(urls.at(-1)!.searchParams.get("selection")).toBeNull();
  await inspector
    .getByRole("button", { name: "Inspect origin proof", exact: true })
    .click();
  await expect(inspector.getByRole("alert")).toHaveText(
    "Companion proof requested",
  );
  expect(proofs[0].searchParams.get("selection")).toBeNull();
  expect(proofs[0].searchParams.get("at_snapshot")).toBe(traceView().token);
  expect(state.commands).toHaveLength(0);
});
