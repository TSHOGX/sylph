import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./fixtures/test";
import type {
  ObjectDetail,
  ResourceObservation,
  ResourceState,
} from "../lib/trace-types";
import {
  agentLink,
  installTraceFixture,
  message,
  openTrace,
  payload,
  tracePage,
  traceView,
} from "./fixtures/trace";

function detail(reference: string): ObjectDetail {
  return {
    view: traceView(),
    reference,
    title: "Retained resource",
    owner: agentLink(),
    execution: null,
    availability: { kind: "available" },
    content: {
      kind: "resource",
      resourceKind: { kind: "goal" },
      observations: [],
      observationsCursor: null,
    },
    relations: [],
    relationsCursor: null,
    coverage: [],
  };
}
function observation(index: number, state: ResourceState): ResourceObservation {
  return {
    claimRef: `claim-${index}`,
    orderBasis: "explicit_order",
    state,
    source: {
      source: {
        issuer: { kind: "environment", value: "fixture" },
        instance: "native",
      },
      generation: 0,
      locator: { kind: "frame", ordinal: index },
    },
    payloads: [],
  };
}
for (const width of [1280, 390]) {
  test(`resource states and explicit payloads remain accessible at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    const row = message(0, "Inspect persistent work");
    const fixture = await installTraceFixture(page, [row]);
    const state = detail(row.anchor);
    if (state.content.kind !== "resource") throw new Error("resource fixture");
    state.content.observations = [
      observation(0, { kind: "schedule_registered" }),
      observation(1, {
        kind: "schedule_fired",
        firingKey: ["first"],
        delivery: "firing-delivery",
      }),
      observation(2, { kind: "schedule_cancelled" }),
      observation(3, { kind: "goal_updated", status: "paused" }),
      observation(4, { kind: "unknown", reason: "No terminal observation" }),
    ];
    state.content.observations[0].payloads = [
      { ...payload("registration", row.anchor), representation: "runtime" },
    ];
    state.relations = [
      { target: "creator", label: "Creating operation", agent: null },
    ];
    fixture.details.set(row.anchor, state);
    fixture.details.set("firing-delivery", {
      ...detail("firing-delivery"),
      title: "Firing delivery",
    });
    fixture.bodies.set("registration", "Observed schedule registration bytes");
    await openTrace(page);
    await page.getByRole("button", { name: "Evidence and details" }).click();
    await expect(
      page.getByRole("heading", { name: "Schedule registered", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Schedule fired", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Schedule cancelled", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Goal state: paused", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Order between sources is unknown.", { exact: false }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Completed", exact: true }),
    ).toHaveCount(0);
    expect(
      fixture.reads.filter((read) => read.includes("/payloads/registration")),
    ).toHaveLength(0);
    await page
      .getByRole("button", { name: "Read runtime output", exact: true })
      .click();
    await expect(
      page.getByText("Observed schedule registration bytes", { exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await page.screenshot({
      path: `/tmp/sylph-resource-inspector-${width}.png`,
      fullPage: true,
    });
    await page
      .getByRole("button", { name: "Open firing delivery", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "Firing delivery", exact: true }),
    ).toBeVisible();
  });
}

test("resource observation pages keep one page mounted and retry the exact failed cursor", async ({
  page,
}) => {
  const row = message(0, "Inspect resource history");
  const fixture = await installTraceFixture(page, [row]);
  const state = detail(row.anchor);
  if (state.content.kind !== "resource") throw new Error("resource fixture");
  state.content.observations = [
    observation(0, { kind: "goal_updated", status: "initial" }),
  ];
  state.content.observationsCursor = "next-page";
  fixture.details.set(row.anchor, state);
  let attempts = 0;
  const requests: URL[] = [];
  await page.route(
    (url) => url.pathname === `/api/v1/trace/objects/${row.anchor}/relations`,
    async (route) => {
      const url = new URL(route.request().url());
      requests.push(url);
      expect(url.searchParams.get("kind")).toBe("observations");
      expect(url.searchParams.get("limit")).toBe("16");
      expect(url.searchParams.get("at_snapshot")).toBe(traceView().token);
      if (url.searchParams.has("before"))
        return route.fulfill({
          json: {
            ...tracePage([
              observation(0, { kind: "goal_updated", status: "initial" }),
            ]),
            afterCursor: "next-page",
            hasAfter: true,
          },
        });
      attempts += 1;
      if (attempts === 1)
        return route.fulfill({
          status: 502,
          json: {
            error: {
              code: "FIXTURE",
              message: "Observation page temporarily unavailable",
            },
          },
        });
      if (attempts === 2)
        return route.fulfill({
          json: {
            ...tracePage<ResourceObservation>([]),
            state: "work_budget_exhausted",
            hasAfter: true,
            afterCursor: "scan-continued",
          },
        });
      return route.fulfill({
        json: {
          ...tracePage(
            Array.from({ length: 16 }, (_, index) =>
              observation(index + 1, {
                kind: "goal_updated",
                status: `page-${index}`,
              }),
            ),
          ),
          beforeCursor: "previous-page",
          hasBefore: true,
        },
      });
    },
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Evidence and details" }).click();
  await expect(
    page.getByRole("heading", { name: "Goal state: initial", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "More state observations", exact: true })
    .click();
  await expect(
    page.getByRole("region", { name: "Observed states" }).getByRole("alert"),
  ).toContainText("temporarily unavailable");
  await page
    .getByRole("button", { name: "Retry this read", exact: true })
    .click();
  await expect(
    page.getByText("More results require another page", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("No resource state observed in this view.", { exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "More state observations", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Goal state: page-0", exact: true }),
  ).toBeVisible();
  expect(requests[0].href).toBe(requests[1].href);
  await expect(
    page.getByRole("heading", { name: "Goal state: initial", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("region", { name: "Observed states" }).getByRole("listitem"),
  ).toHaveCount(16);
  await page
    .getByRole("button", { name: "Previous state observations", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Goal state: initial", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Observed states" }).getByRole("listitem"),
  ).toHaveCount(1);
});

test("revision details page retained parts without preloading bodies or reusing an expanded page", async ({
  page,
}) => {
  const row = message(0, "Inspect recorded wrapper");
  const fixture = await installTraceFixture(page, [row]);
  const access = {
    ...payload("wrapper", row.anchor),
    representation: "conversation" as const,
  };
  fixture.details.set(row.anchor, {
    ...detail(row.anchor),
    title: "Recorded wrapper",
    content: {
      kind: "revision",
      representation: "conversation",
      form: { kind: "append", predecessor: "previous-revision" },
      payloads: [access],
      payloadsCursor: "next-part",
    },
  });
  fixture.details.set("previous-revision", {
    ...detail("previous-revision"),
    title: "Previous retained revision",
  });
  fixture.bodies.set("wrapper", "The exact observed vendor wrapper");
  await page.route(
    (url) => url.pathname === `/api/v1/trace/objects/${row.anchor}/relations`,
    async (route) => {
      const url = new URL(route.request().url());
      expect(url.searchParams.get("kind")).toBe("payloads");
      return route.fulfill({
        json: {
          ...tracePage([access]),
          beforeCursor: "previous-part",
          hasBefore: true,
        },
      });
    },
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Evidence and details" }).click();
  await expect(
    page.getByRole("heading", {
      name: "Recorded conversation result",
      exact: true,
    }),
  ).toBeVisible();
  expect(
    fixture.reads.filter((read) => read.includes("/payloads/wrapper")),
  ).toHaveLength(0);
  await page
    .getByRole("button", {
      name: "Read recorded conversation result",
      exact: true,
    })
    .click();
  await expect(
    page.getByText("The exact observed vendor wrapper", { exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "More retained parts", exact: true })
    .click();
  await expect(
    page.getByRole("button", {
      name: "Read recorded conversation result",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByText("The exact observed vendor wrapper", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText(
      "Recorded append segment; earlier content is in the preceding revision.",
      { exact: true },
    ),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Open preceding revision", exact: true })
    .click();
  await expect(
    page.getByRole("heading", {
      name: "Previous retained revision",
      exact: true,
    }),
  ).toBeVisible();
});
