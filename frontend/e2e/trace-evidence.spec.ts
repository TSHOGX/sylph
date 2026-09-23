import { expect, test, type Page } from "./fixtures/test";
import {
  installTraceFixture,
  message,
  openTrace,
  traceLimits,
  traceView,
} from "./fixtures/trace";

async function setup(page: Page) {
  const row = message(0, "Content with retained provenance");
  await installTraceFixture(page, [row]);
  const calls: string[] = [];
  let failNext = true;
  const summaries = Array.from({ length: 18 }, (_, index) => ({
    claimRef: `claim-${index}`,
    preview: `Observed interpretation ${index}`,
    previewTruncated: false,
    factKind: "declaration",
    mapperBuild: "vendor-mapper-1",
    ruleId: `retained-rule-${index}`,
    evidenceCount: 1,
    detailBytes: 256,
  }));
  await page.route(
    (url) => url.pathname.endsWith(`/objects/${row.anchor}/evidence`),
    async (route) => {
      const url = new URL(route.request().url());
      calls.push(url.pathname + url.search);
      expect(url.searchParams.get("at_snapshot")).toBe(traceView().token);
      const cursor = url.searchParams.get("after");
      if (cursor === "next-8" && failNext) {
        failNext = false;
        return route.fulfill({
          status: 503,
          json: {
            error: {
              code: "trace_unavailable",
              message: "Temporary evidence read failure",
            },
          },
        });
      }
      const offset = cursor ? Number(cursor.slice(5)) : 0;
      const filtered =
        url.searchParams.get("corrects") === "prior"
          ? [summaries[0]]
          : summaries;
      const items = filtered.slice(offset, offset + 8);
      await route.fulfill({
        json: {
          view: traceView(),
          items,
          beforeCursor: null,
          afterCursor:
            offset + 8 < filtered.length ? `next-${offset + 8}` : null,
          hasBefore: false,
          hasAfter: offset + 8 < filtered.length,
          returnedItems: items.length,
          returnedBytes: 1024,
          total: {
            value: null,
            scope: "authorized retained claims",
            complete: false,
          },
          state: offset + 8 < filtered.length ? "more" : "complete",
          limits: traceLimits,
        },
      });
    },
  );
  await page.route(
    (url) => url.pathname.includes(`/objects/${row.anchor}/claims/`),
    async (route) => {
      const url = new URL(route.request().url());
      calls.push(url.pathname + url.search);
      expect(url.searchParams.get("at_snapshot")).toBe(traceView().token);
      if (url.pathname.includes("/sources/")) {
        const start = Number(url.searchParams.get("start") ?? 0);
        const body = Buffer.from(
          start === 0
            ? ' {"same":1,"same":2} \r\n'
            : "<script>globalThis.sourceExecuted = true</script>",
        );
        return route.fulfill({
          json: {
            view: traceView(),
            capture: {
              observationRef: {},
              sourcePosition: {
                source: {
                  issuer: { kind: "environment", value: "fixture" },
                  instance: "retained-source",
                },
                generation: 0,
                locator: { kind: "frame", ordinal: 3 },
              },
              capturedAt: {
                domain: "fixture.clock",
                value: 12,
                unit: "milliseconds",
                basis: "source_wall",
              },
              sourceClock: null,
              transportKind: "native_jsonl",
              vendorBuild: "captured-vendor",
            },
            start,
            endExclusive: start + body.length,
            totalBytes: start === 0 ? body.length + 47 : start + body.length,
            coverage: { kind: "complete" },
            bytesBase64: body.toString("base64"),
          },
        });
      }
      const claim = url.pathname.split("/").at(-1)!;
      return route.fulfill({
        json: {
          view: traceView(),
          subject: row.anchor,
          claim: {
            claimRef: claim,
            fact: {
              kind: "declare",
              object: {
                kind: "message",
                author: { kind: "unknown" },
                note:
                  claim === "prior"
                    ? "Earlier interpretation"
                    : "Current interpretation",
              },
            },
            evidence: [{}],
            ruleId: "retained-rule",
            mapperBuild: "vendor-mapper-1",
            supersedes: claim === "prior" ? [] : ["prior"],
            sourceRevision: null,
          },
          related: [],
          sources: ["observation-one"],
        },
      });
    },
  );
  await openTrace(page);
  await page
    .getByRole("button", { name: "Evidence and details", exact: true })
    .click();
  return { calls };
}

test("evidence is lazy, pages remain bounded and a failed page retries its exact cursor", async ({
  page,
}) => {
  const { calls } = await setup(page);
  expect(calls).toHaveLength(0);
  await page
    .getByRole("button", { name: "Inspect retained evidence", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Inspect claim", exact: true }),
  ).toHaveCount(8);
  expect(calls.some((call) => call.includes("/claims/"))).toBe(false);
  await page.getByRole("button", { name: "More results", exact: true }).click();
  await expect(page.getByText("Temporary evidence read failure")).toBeVisible();
  await page
    .getByRole("button", { name: "Retry this read", exact: true })
    .click();
  await expect(
    page.getByText("declaration · retained-rule-8 · vendor-mapper-1", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByText("declaration · retained-rule-0 · vendor-mapper-1", {
      exact: true,
    }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Inspect claim", exact: true }),
  ).toHaveCount(8);
  expect(calls.filter((call) => call.includes("after=next-8"))).toHaveLength(2);
});

test("claim inspection separates corrections from exact source ranges and renders source bytes as text", async ({
  page,
}) => {
  const { calls } = await setup(page);
  await page
    .getByRole("button", { name: "Inspect retained evidence", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Inspect claim", exact: true })
    .first()
    .click();
  await expect(page.getByText(/Current interpretation/)).toBeVisible();
  expect(calls.some((call) => call.includes("/sources/"))).toBe(false);
  await page
    .getByRole("button", { name: "Read retained source 1", exact: true })
    .click();
  await expect(page.locator("[data-evidence-bytes]")).toHaveText(
    ' {"same":1,"same":2} \r\n',
  );
  expect(await page.locator("[data-evidence-bytes]").textContent()).toBe(
    ' {"same":1,"same":2} \r\n',
  );
  await expect(page.getByText(/retained-source/)).toBeVisible();
  await page.getByRole("button", { name: "Next range", exact: true }).click();
  await expect(page.locator("[data-evidence-bytes]")).toContainText("<script>");
  expect(
    await page.evaluate(
      () => (window as unknown as { sourceExecuted?: boolean }).sourceExecuted,
    ),
  ).toBeUndefined();
  await page
    .getByRole("button", { name: "Explicitly corrects 1", exact: true })
    .click();
  await expect(page.getByText(/Earlier interpretation/)).toBeVisible();
  await expect(page.locator("[data-evidence-bytes]")).toHaveCount(0);
  await page
    .getByRole("button", { name: "Later corrections", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Inspect claim", exact: true }),
  ).toHaveCount(1);
  expect(calls.some((call) => call.includes("corrects=prior"))).toBe(true);
});

test("large evidence facts and observation lists stay bounded on mobile and revoked access clears source bytes", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await setup(page);
  let sourceReads = 0;
  await page.route(
    (url) =>
      url.pathname.includes("/claims/") && url.pathname.includes("/sources/"),
    async (route) => {
      sourceReads++;
      if (sourceReads > 1)
        return route.fulfill({
          status: 403,
          json: {
            error: {
              code: "permission_denied",
              message: "Evidence permission revoked",
            },
          },
        });
      return route.fulfill({
        json: {
          view: traceView(),
          maxRangeBytes: 2,
          capture: {
            observationRef: {},
            sourcePosition: {
              source: {
                issuer: { kind: "environment", value: "fixture" },
                instance: "binary-source",
              },
              generation: 0,
              locator: { kind: "frame", ordinal: 1 },
            },
            capturedAt: {},
            sourceClock: null,
            transportKind: "live_stdout",
            vendorBuild: null,
          },
          start: 0,
          endExclusive: 2,
          totalBytes: 4,
          coverage: { kind: "complete" },
          bytesBase64: Buffer.from([0, 255]).toString("base64"),
        },
      });
    },
  );
  await page.route(
    (url) => url.pathname.endsWith("/claims/claim-0"),
    (route) =>
      route.fulfill({
        json: {
          view: traceView(),
          subject: "row",
          claim: {
            claimRef: "claim-0",
            fact: { kind: "diagnostic", text: "🙂".repeat(50_000) },
            evidence: [],
            ruleId: "large-explicit-fact",
            mapperBuild: "test",
            supersedes: [],
            sourceRevision: null,
          },
          related: [],
          sources: Array.from({ length: 40 }, (_, index) => `source-${index}`),
        },
      }),
  );
  await page
    .getByRole("button", { name: "Inspect retained evidence", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Inspect claim", exact: true })
    .first()
    .click();
  await expect(
    page.getByRole("button", { name: /^Read retained source \d+$/ }),
  ).toHaveCount(8);
  const fact = page.locator("[data-evidence-fact]");
  await expect
    .poll(async () => (await fact.textContent())?.length ?? 0)
    .toBeGreaterThan(0);
  expect((await fact.textContent())!.length).toBeLessThanOrEqual(
    traceLimits.maxPayloadBytes,
  );
  await page
    .getByRole("button", { name: "Next fact range", exact: true })
    .click();
  expect((await fact.textContent())!.length).toBeLessThanOrEqual(
    traceLimits.maxPayloadBytes,
  );
  expect((await fact.textContent())!).not.toContain("\ufffd");
  await page
    .getByRole("button", { name: "Read retained source 1", exact: true })
    .click();
  await expect(page.locator("[data-evidence-bytes]")).toHaveText("00 ff");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.getByRole("button", { name: "Next range", exact: true }).click();
  await expect(page.locator("[data-evidence-bytes]")).toHaveCount(0);
});
