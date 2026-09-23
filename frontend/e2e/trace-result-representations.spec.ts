import { expect, test } from "./fixtures/test";
import type {
  ResultOccurrence,
  ResultRepresentationEntry,
  ResultRepresentation,
} from "../lib/trace-types";
import {
  agentLink,
  installTraceFixture,
  message,
  openTrace,
  payload,
  resultCatalogue,
  traceLimits,
  traceView,
} from "./fixtures/trace";

function occurrence(
  index: number,
  representation: ResultRepresentation,
  reference: string,
): ResultOccurrence {
  return {
    reference: `${representation}-${index}`,
    representation,
    disposition: "final_return",
    label: `${representation} occurrence ${index}`,
    availability: { kind: "available" },
    payloads: [
      { ...payload(`${representation}-${index}`, reference), representation },
    ],
  };
}
async function fixture(
  page: Parameters<typeof installTraceFixture>[0],
  catalogue: ResultRepresentationEntry[],
) {
  const row = message(0, "Inspect result representations");
  const state = await installTraceFixture(page, [row]);
  state.details.set(row.anchor, {
    view: traceView(),
    reference: row.anchor,
    title: "Recorded operation",
    owner: agentLink(),
    execution: null,
    availability: { kind: "available" },
    content: {
      kind: "operation",
      category: { kind: "wait" },
      input: [],
      resultRepresentations: catalogue,
      resources: [],
      resourcesCursor: null,
    },
    relations: [],
    relationsCursor: null,
    coverage: [],
  });
  return { row, state };
}
function queries(reads: string[], kind: string) {
  return reads
    .map((read) => new URL(read.slice(4), "http://fixture"))
    .filter((url) => url.searchParams.get("kind") === kind);
}

for (const width of [1280, 390]) {
  test(`result selection and comparison fetch only their bounded representations at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const catalogue = resultCatalogue(["runtime", "caller_result"]);
    catalogue.find((entry) => entry.representation === "conversation")!.state =
      {
        kind: "unavailable",
        reason: "Recorded conversation bytes were not retained",
      };
    const { row, state } = await fixture(page, catalogue);
    const runtime = Array.from(
      { length: traceLimits.maxItems + 5 },
      (_, index) => occurrence(index, "runtime", row.anchor),
    );
    const caller = occurrence(0, "caller_result", row.anchor);
    const conversation = {
      ...occurrence(0, "conversation", row.anchor),
      payloads: [],
      availability: {
        kind: "unavailable" as const,
        reason: "Only the result reference survived",
      },
    };
    state.results.set(row.anchor, [...runtime, caller, conversation]);
    state.bodies.set("caller_result-0", "Exact caller bytes");
    state.bodies.set("runtime-0", "Exact runtime bytes");
    await openTrace(page);
    await page
      .getByRole("button", { name: "Evidence and details", exact: true })
      .click();
    const choice = page.getByRole("combobox", {
      name: "Representation",
      exact: true,
    });
    await expect(choice).toHaveValue("caller_result");
    const callerPanel = page.getByRole("region", {
      name: "Returned to caller",
      exact: true,
    });
    await expect(callerPanel.getByRole("heading", { level: 5 })).toHaveCount(1);
    expect(
      queries(state.reads, "results").map((url) =>
        url.searchParams.get("representation"),
      ),
    ).toEqual(["caller_result"]);
    expect(
      state.reads.filter((read) => read.includes("/payloads/")),
    ).toHaveLength(0);
    await callerPanel
      .getByRole("button", { name: "Read returned to caller", exact: true })
      .click();
    await expect(
      callerPanel.getByText("Exact caller bytes", { exact: true }),
    ).toBeVisible();

    const firstPage = page.waitForResponse((response) => {
      const url = new URL(response.url());
      return (
        url.searchParams.get("kind") === "results" &&
        url.searchParams.get("representation") === "runtime"
      );
    });
    await choice.selectOption("runtime");
    const initial = await (await firstPage).json();
    const runtimePanel = page.getByRole("region", {
      name: "Runtime output",
      exact: true,
    });
    await expect(runtimePanel.getByRole("heading", { level: 5 })).toHaveCount(
      initial.returnedItems,
    );
    await expect(
      page.getByText("Exact caller bytes", { exact: true }),
    ).toHaveCount(0);
    await runtimePanel
      .getByRole("button", { name: "Read runtime output", exact: true })
      .first()
      .click();
    await expect(
      runtimePanel.getByText("Exact runtime bytes", { exact: true }),
    ).toBeVisible();
    const nextPage = page.waitForResponse((response) => {
      const url = new URL(response.url());
      return (
        url.searchParams.get("kind") === "results" &&
        url.searchParams.get("after") === initial.afterCursor
      );
    });
    await runtimePanel
      .getByRole("button", { name: "Next page", exact: true })
      .click();
    const next = await (await nextPage).json();
    await expect(runtimePanel.getByRole("heading", { level: 5 })).toHaveCount(
      next.returnedItems,
    );
    await expect(
      runtimePanel.getByText("runtime occurrence 0", { exact: false }),
    ).toHaveCount(0);
    await expect(
      page.getByText("Exact runtime bytes", { exact: true }),
    ).toHaveCount(0);
    await runtimePanel
      .getByRole("button", { name: "Previous page", exact: true })
      .click();
    await expect(
      runtimePanel.getByText("runtime occurrence 0", { exact: false }),
    ).toBeVisible();

    await choice.selectOption("conversation");
    await expect(
      page.getByText("Recorded conversation bytes were not retained", {
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByText("Only the result reference survived", { exact: true }),
    ).toBeVisible();
    await expect(choice).toHaveValue("conversation");
    await expect(runtimePanel).toHaveCount(0);
    await choice.selectOption("runtime");
    await page
      .getByRole("checkbox", { name: "Compare representations", exact: true })
      .check();
    await expect(choice).toHaveCount(0);
    await page
      .getByRole("checkbox", { name: "Returned to caller", exact: true })
      .check();
    await expect(callerPanel.getByRole("heading", { level: 5 })).toHaveCount(1);
    await expect(runtimePanel.getByRole("heading", { level: 5 })).toHaveCount(
      initial.returnedItems,
    );
    expect(
      state.reads.filter((read) => read.includes("/payloads/")),
    ).toHaveLength(2);
    await page
      .getByRole("checkbox", { name: "Runtime output", exact: true })
      .uncheck();
    await expect(runtimePanel).toHaveCount(0);
    expect(
      queries(state.reads, "results").every(
        (url) => url.searchParams.get("at_snapshot") === traceView().token,
      ),
    ).toBe(true);
    expect(state.commands).toHaveLength(0);
    const size = await page.evaluate(() => ({
      width: document.documentElement.clientWidth,
      scroll: document.documentElement.scrollWidth,
    }));
    expect(size.scroll).toBeLessThanOrEqual(size.width);
    await testInfo.attach(`result-comparison-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
  });
}

test("availability continuation retries exactly once per action and preserves an explicit choice", async ({
  page,
}) => {
  const catalogue = resultCatalogue(["runtime"]);
  catalogue.find((entry) => entry.representation === "caller_result")!.state = {
    kind: "pending",
    phase: "payload",
    cursor: "presence-one",
  };
  const { row, state } = await fixture(page, catalogue);
  state.results.set(row.anchor, [
    occurrence(0, "runtime", row.anchor),
    occurrence(0, "caller_result", row.anchor),
  ]);
  const probes: string[] = [];
  let release!: () => void;
  const responseReady = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(
    (url) => url.searchParams.get("kind") === "result_representations",
    async (route) => {
      const url = new URL(route.request().url());
      probes.push(url.href);
      if (probes.length === 1)
        return route.fulfill({
          status: 502,
          json: {
            error: {
              code: "PROBE_FAILED",
              message: "Availability read failed",
            },
          },
        });
      if (url.searchParams.get("after") === "presence-one")
        return route.fulfill({
          json: {
            representation: "caller_result",
            state: {
              kind: "pending",
              phase: "occurrence",
              cursor: "presence-two",
            },
          },
        });
      await responseReady;
      return route.fulfill({
        json: { representation: "caller_result", state: { kind: "available" } },
      });
    },
  );
  try {
    await openTrace(page);
    await page
      .getByRole("button", { name: "Evidence and details", exact: true })
      .click();
    const choice = page.getByRole("combobox", {
      name: "Representation",
      exact: true,
    });
    await expect(choice).toHaveValue("runtime");
    await choice.selectOption("conversation");
    await choice.selectOption("runtime");
    await page
      .getByRole("checkbox", { name: "Compare representations", exact: true })
      .check();
    await expect(choice).toHaveCount(0);
    await page
      .getByRole("checkbox", { name: "Returned to caller", exact: true })
      .check();
    const callerPanel = page.getByRole("region", {
      name: "Returned to caller",
      exact: true,
    });
    await callerPanel
      .getByRole("button", { name: "More result occurrences", exact: true })
      .click();
    await expect(callerPanel.getByRole("alert")).toHaveText(
      "Availability read failed",
    );
    await callerPanel
      .getByRole("button", { name: "Retry this read", exact: true })
      .click();
    const more = callerPanel.getByRole("button", {
      name: "More result occurrences",
      exact: true,
    });
    await expect(more).toBeEnabled();
    expect(probes).toHaveLength(2);
    expect(probes[1]).toBe(probes[0]);
    await more.click();
    await expect.poll(() => probes.length).toBe(3);
    await expect(more).toBeDisabled();
    expect(new URL(probes[2]).searchParams.get("after")).toBe("presence-two");
    expect(new URL(probes[2]).searchParams.get("result_phase")).toBe(
      "occurrence",
    );
    expect(
      queries(state.reads, "results").some(
        (url) => url.searchParams.get("representation") === "caller_result",
      ),
    ).toBe(false);
    release();
    await expect(callerPanel.getByRole("heading", { level: 5 })).toHaveCount(1);
    await page
      .getByRole("checkbox", { name: "Compare representations", exact: true })
      .uncheck();
    await expect(choice).toHaveValue("runtime");
    expect(
      state.reads.filter((read) => read.includes("/payloads/")),
    ).toHaveLength(0);
  } finally {
    release();
  }
});

test("an empty caller result remains selected and reads an authorized zero-byte range", async ({
  page,
}) => {
  const { row, state } = await fixture(
    page,
    resultCatalogue(["runtime", "caller_result"]),
  );
  const empty = occurrence(0, "caller_result", row.anchor);
  empty.payloads[0].totalBytes = 0;
  state.results.set(row.anchor, [occurrence(0, "runtime", row.anchor), empty]);
  state.bodies.set("caller_result-0", "");
  await openTrace(page);
  await page
    .getByRole("button", { name: "Evidence and details", exact: true })
    .click();
  const panel = page.getByRole("region", {
    name: "Returned to caller",
    exact: true,
  });
  await panel
    .getByRole("button", { name: "Read returned to caller", exact: true })
    .click();
  await expect(panel.getByText(/bytes 0–0 of 0/)).toBeVisible();
  await expect(panel.getByRole("status")).toHaveCount(0);
  const read = state.reads.find((read) =>
    read.includes("/payloads/caller_result-0"),
  );
  expect(read).toBeDefined();
  const url = new URL(read!.slice(4), "http://fixture");
  expect(url.searchParams.get("start")).toBe("0");
  expect(url.searchParams.get("end")).toBe("0");
  await expect(
    page.getByRole("combobox", { name: "Representation", exact: true }),
  ).toHaveValue("caller_result");
});
