import { expect, test } from "./fixtures/test";
import type {
  ObjectDetail,
  PartSummary,
  ResultOccurrence,
  TraceRelation,
} from "../lib/trace-types";
import {
  agentLink,
  installTraceFixture,
  message,
  openTrace,
  payload,
  tracePage,
  traceView,
  resultCatalogue,
} from "./fixtures/trace";

for (const family of ["parts", "relations", "results"] as const) {
  for (const failure of ["first", "later"] as const) {
    test(`${family} retries its ${failure} failed page at the same snapshot and cursor`, async ({
      page,
    }) => {
      const row = message(0, "Initially retained content");
      if (row.content.kind !== "message") throw new Error("Message fixture");
      const selection = "selected-branch";
      row.content.parts[0].payload.selection = selection;
      row.content.partsTotal = 3;
      row.content.partsComplete = family !== "parts";
      row.content.partsCursor = family === "parts" ? "page-one" : null;
      const state = await installTraceFixture(page, [row]);
      state.mainHeader.history = {
        ...state.mainHeader.history,
        selection,
        label: "Selected branch",
        isCurrent: false,
      };
      const view = traceView();
      const detail: ObjectDetail = {
        view,
        reference: row.anchor,
        title: "Retained detail",
        owner: agentLink(),
        execution: null,
        availability: { kind: "available" },
        content:
          family === "results"
            ? {
                kind: "operation",
                category: {
                  kind: "tool",
                  namespace: null,
                  name: "fixture-tool",
                },
                input: [],
                resultRepresentations: resultCatalogue(["caller_result"]),
                resources: [],
                resourcesCursor: null,
              }
            : { kind: "generic", label: "Retained detail", payloads: [] },
        relations: [],
        relationsCursor: family === "relations" ? "page-one" : null,
        coverage: [],
      };
      state.details.set(row.anchor, detail);
      const reference =
        family === "parts" ? row.content.revisionRef : row.anchor;
      const endpoint = `/api/v1/trace/objects/${reference}/${family === "parts" ? "parts" : "relations"}`;
      const requests: string[] = [];
      const failCursor = failure === "first" ? "page-one" : "page-two";
      let releaseRetry!: () => void;
      const retryResponse = new Promise<void>((resolve) => {
        releaseRetry = resolve;
      });
      const expectedLabel =
        failure === "first" ? "Loaded page one" : "Loaded page two";
      await page.route(
        (url) => url.pathname === endpoint,
        async (route) => {
          const url = new URL(route.request().url());
          requests.push(url.href);
          const cursor =
            url.searchParams.get("after") ??
            (family === "results" ? "page-one" : null);
          const attempt = requests.filter(
            (request) => request === url.href,
          ).length;
          if (cursor === failCursor && attempt === 1)
            return route.fulfill({
              status: 502,
              json: {
                error: {
                  code: "PAGE_READ_FAILED",
                  message: "Retained detail page unavailable",
                },
              },
            });
          if (cursor === failCursor) await retryResponse;
          const index = cursor === "page-one" ? 1 : 2;
          const label = index === 1 ? "Loaded page one" : "Loaded page two";
          const item: PartSummary | TraceRelation | ResultOccurrence =
            family === "parts"
              ? {
                  partRef: `part-${index}`,
                  kind: "text",
                  preview: label,
                  previewTruncated: false,
                  payload: {
                    ...payload(`body-${index}`, row.anchor),
                    selection,
                  },
                }
              : family === "relations"
                ? {
                    target: `related-${index}`,
                    containingAgent: agentLink().address,
                    label,
                    agent: null,
                  }
                : {
                    reference: `result-${index}`,
                    disposition: "final_return",
                    representation: "caller_result",
                    label,
                    payloads: [],
                    availability: { kind: "available" },
                  };
          const result = tracePage([item], view);
          result.afterCursor = index === 1 ? "page-two" : null;
          result.hasAfter = index === 1;
          return route.fulfill({ json: result });
        },
      );
      try {
        await openTrace(page);
        if (family !== "parts")
          await page
            .getByRole("button", { name: "Evidence and details", exact: true })
            .click();
        const more = page.getByRole("button", {
          name:
            family === "parts"
              ? /^More message parts/
              : family === "relations"
                ? /^More results · relations$/
                : /^Next page$/,
        });
        if (family !== "results") await more.click();
        if (failure === "later") {
          await expect(page.getByText(/Loaded page one/).first()).toBeVisible();
          await more.click();
        }
        await expect(
          page
            .getByRole("alert")
            .filter({ hasText: "Retained detail page unavailable" }),
        ).toBeVisible();
        if (failure === "later")
          await expect(page.getByText(/Loaded page one/).first()).toBeVisible();
        const failedRequest = requests.at(-1)!;
        const beforeRetry = requests.length;
        await page
          .getByRole("button", { name: "Retry this read", exact: true })
          .click();
        await expect.poll(() => requests.length).toBe(beforeRetry + 1);
        expect(requests.at(-1)).toBe(failedRequest);
        const retried = new URL(requests.at(-1)!);
        expect(retried.searchParams.get("at_snapshot")).toBe(view.token);
        expect(retried.searchParams.get("selection")).toBe(selection);
        expect(retried.searchParams.get("after")).toBe(
          family === "results" && failure === "first" ? null : failCursor,
        );
        await expect(more).toBeDisabled();
        if (failure === "later")
          await expect(page.getByText(/Loaded page one/).first()).toBeVisible();
        releaseRetry();
        await expect(
          page.getByText(new RegExp(expectedLabel)).first(),
        ).toBeVisible();
        await expect(
          page
            .getByRole("alert")
            .filter({ hasText: "Retained detail page unavailable" }),
        ).toHaveCount(0);
        if (failure === "later") await expect(more).toBeDisabled();
      } finally {
        releaseRetry();
      }
    });
  }
}
