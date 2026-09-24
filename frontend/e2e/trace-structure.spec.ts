import { expect, test } from "./fixtures/test";
import type { StructureNode, StructureStatus } from "../lib/trace-types";
import {
  installTraceFixture,
  message,
  openTrace,
  traceView,
} from "./fixtures/trace";

const limits = {
  maxJobs: 4,
  maxBytes: 268435456,
  maxNodes: 1000000,
  maxDepth: 128,
  maxIndexBytes: 134217728,
  chunkBytes: 65536,
  pageItems: 32,
  previewBytes: 256,
  maxQuerySteps: 20000,
  lifetimeMs: 120000,
};
function node(
  id: number,
  ordinal: number,
  kind: StructureNode["kind"],
  children = 0,
): StructureNode {
  return {
    id,
    parent: id === 1 ? null : 1,
    ordinal,
    kind,
    children,
    start: ordinal * 10,
    end: ordinal * 10 + 9,
    name:
      id === 1 ? null : { text: '"same"', truncated: false, start: 0, end: 6 },
    preview:
      kind === "number"
        ? { text: "9007199254740993", truncated: false, start: 0, end: 16 }
        : null,
  };
}
function status(state: "building" | "ready" | "failed"): StructureStatus {
  return {
    id: "structure-one",
    view: traceView(),
    state,
    error: null,
    bytesRead: 12,
    totalBytes: 50000000,
    nodes: 12,
    expiresAtMs: Date.now() + 120000,
    limits,
  };
}

for (const width of [1280, 390]) {
  test(`JSON exploration pages duplicate members and restores Back without eager bodies at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const row = message(0, "Recorded JSON");
    if (row.content.kind !== "message") throw new Error("message fixture");
    row.content.parts[0].payload.mediaType = "application/json";
    const fixture = await installTraceFixture(page, [row]);
    const reads: URL[] = [],
      creates: URL[] = [];
    let closed = 0;
    let release!: () => void;
    const prepared = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/api/v1/trace/payloads/*/structure?*", (route) => {
      creates.push(new URL(route.request().url()));
      return route.fulfill({ status: 202, json: status("building") });
    });
    await page.route(
      "**/api/v1/trace/structures/structure-one*",
      async (route) => {
        if (route.request().method() === "DELETE") {
          closed++;
          return route.fulfill({ status: 204 });
        }
        const url = new URL(route.request().url());
        reads.push(url);
        await prepared;
        const parent = Number(url.searchParams.get("node"));
        const after = Number(url.searchParams.get("after") ?? -1);
        const result = status("ready");
        result.node =
          parent === 1 ? node(1, 0, "object", 10000) : node(2, 0, "array", 1);
        result.items =
          parent === 1
            ? Array.from({ length: limits.pageItems }, (_, i) =>
                node(
                  i + after + 3,
                  i + after + 1,
                  i === 0 && after === -1 ? "array" : "object",
                  i === 0 && after === -1 ? 1 : 0,
                ),
              )
            : [node(900, 0, "number")];
        result.after = parent === 1 ? after + limits.pageItems : null;
        return route.fulfill({ json: result });
      },
    );
    try {
      await openTrace(page);
      expect(creates).toHaveLength(0);
      await page
        .getByRole("button", { name: "Explore JSON", exact: true })
        .click();
      const explorer = page.getByRole("region", {
        name: "Structured content",
        exact: true,
      });
      await expect(explorer.getByRole("status")).toContainText("Indexing");
      await expect.poll(() => reads.length).toBe(1);
      expect(
        fixture.reads.filter((read) => read.includes("/payloads/")),
      ).toHaveLength(0);
      expect(creates[0].searchParams.get("revision")).toBe(
        row.content.parts[0].payload.bodyRevision,
      );
      release();
      await expect(explorer.getByRole("listitem")).toHaveCount(
        limits.pageItems,
      );
      await expect(explorer.getByText('"same"', { exact: true })).toHaveCount(
        limits.pageItems,
      );
      await explorer
        .getByRole("button", { name: "Open array (1)", exact: true })
        .click();
      await expect(explorer.getByRole("listitem")).toHaveCount(1);
      await expect(
        explorer.getByRole("heading", {
          name: "Structured content",
          exact: true,
        }),
      ).toBeFocused();
      await expect(
        explorer.getByText("9007199254740993", { exact: true }),
      ).toBeVisible();
      await explorer
        .getByRole("button", {
          name: "Back to previous structure page",
          exact: true,
        })
        .click();
      await expect(explorer.getByRole("listitem")).toHaveCount(
        limits.pageItems,
      );
      await explorer
        .getByRole("button", { name: "Next page", exact: true })
        .click();
      await expect
        .poll(() => reads.at(-1)?.searchParams.get("after"))
        .toBe(String(limits.pageItems - 1));
      await expect(explorer.getByRole("listitem")).toHaveCount(
        limits.pageItems,
      );
      await explorer
        .getByRole("button", {
          name: "Back to previous structure page",
          exact: true,
        })
        .click();
      await expect(
        explorer.getByRole("button", {
          name: "Open array (1)",
          exact: true,
        }),
      ).toBeVisible();
      expect(
        fixture.reads.filter((read) => read.includes("/payloads/")),
      ).toHaveLength(0);
      await testInfo.attach(`structure-${width}`, {
        body: await page.screenshot(),
        contentType: "image/png",
      });
      await explorer
        .getByRole("button", { name: "Close structured content", exact: true })
        .click();
      await expect.poll(() => closed).toBe(1);
      await expect(explorer).toHaveCount(0);
      expect(fixture.commands).toHaveLength(0);
    } finally {
      release();
    }
  });
}

test("closing structured exploration while admission is pending cancels the late job", async ({
  page,
}) => {
  const row = message(0, "JSON source");
  if (row.content.kind !== "message") throw new Error("message fixture");
  row.content.parts[0].payload.mediaType = "application/json";
  await installTraceFixture(page, [row]);
  let entered = false,
    cancelled = 0;
  let release!: () => void;
  const response = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/v1/trace/payloads/*/structure?*", async (route) => {
    entered = true;
    await response;
    return route.fulfill({ status: 202, json: status("building") });
  });
  await page.route("**/api/v1/trace/structures/structure-one", (route) => {
    cancelled++;
    return route.fulfill({ status: 204 });
  });
  try {
    await openTrace(page);
    await page
      .getByRole("button", { name: "Explore JSON", exact: true })
      .click();
    await expect.poll(() => entered).toBe(true);
    await page
      .getByRole("button", { name: "Close structured content", exact: true })
      .click();
    release();
    await expect.poll(() => cancelled).toBe(1);
    await expect(
      page.getByRole("region", { name: "Structured content", exact: true }),
    ).toHaveCount(0);
  } finally {
    release();
  }
});

for (const statusCode of [403, 404, 410]) {
  test(`a revoked structural page (${statusCode}) clears previously visible members and cancels its job`, async ({
    page,
  }) => {
    const row = message(0, "JSON source");
    if (row.content.kind !== "message") throw new Error("message fixture");
    row.content.parts[0].payload.mediaType = "application/json";
    await installTraceFixture(page, [row]);
    let cancelled = 0;
    await page.route("**/api/v1/trace/payloads/*/structure?*", (route) =>
      route.fulfill({ status: 202, json: status("building") }),
    );
    await page.route("**/api/v1/trace/structures/structure-one*", (route) => {
      if (route.request().method() === "DELETE") {
        cancelled++;
        return route.fulfill({ status: 204 });
      }
      if (new URL(route.request().url()).searchParams.has("after"))
        return route.fulfill({
          status: statusCode,
          json: { error: { code: "FORBIDDEN", message: "Access revoked" } },
        });
      const privateNode = node(2, 0, "string");
      privateNode.preview = {
        text: "Private structured value",
        truncated: false,
        start: 0,
        end: 24,
      };
      return route.fulfill({
        json: {
          ...status("ready"),
          node: node(1, 0, "object", 10),
          items: [privateNode],
          after: 0,
        },
      });
    });
    await openTrace(page);
    await page
      .getByRole("button", { name: "Explore JSON", exact: true })
      .click();
    const explorer = page.getByRole("region", {
      name: "Structured content",
      exact: true,
    });
    await expect(
      explorer.getByText("Private structured value", { exact: true }),
    ).toBeVisible();
    await explorer
      .getByRole("button", { name: "Next page", exact: true })
      .click();
    await expect(
      page.getByText("Private structured value", { exact: true }),
    ).toHaveCount(0);
    await expect.poll(() => cancelled).toBe(1);
  });
}

test("large structured values read exact bounded UTF-8 windows only after expansion", async ({
  page,
}) => {
  const row = message(0, "Large structured value");
  if (row.content.kind !== "message") throw new Error("fixture");
  const access = row.content.parts[0].payload;
  access.mediaType = "application/json";
  const document = `["${"中".repeat(8000)}"]`;
  access.totalBytes = Buffer.byteLength(document);
  access.availableRanges = [{ start: 0, endExclusive: access.totalBytes }];
  const fixture = await installTraceFixture(page, [row]);
  fixture.bodies.set(access.payloadRef, document);
  await page.route("**/api/v1/trace/payloads/*/structure?*", (route) =>
    route.fulfill({ status: 202, json: status("building") }),
  );
  await page.route("**/api/v1/trace/structures/structure-one*", (route) => {
    if (route.request().method() === "DELETE")
      return route.fulfill({ status: 204 });
    const scalar = node(2, 0, "string");
    scalar.start = 1;
    scalar.end = Buffer.byteLength(document) - 1;
    scalar.name = null;
    scalar.preview = { text: '"中中', truncated: true, start: 1, end: 8 };
    return route.fulfill({
      json: {
        ...status("ready"),
        node: node(1, 0, "array", 1),
        items: [scalar],
        after: null,
      },
    });
  });
  const boot = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname ===
      "/api/v1/trace/agents/opaque-main-agent",
  );
  await openTrace(page);
  const configured = (await (await boot).json()).window.limits.maxPayloadBytes;
  await page.getByRole("button", { name: "Explore JSON", exact: true }).click();
  const explorer = page.getByRole("region", {
    name: "Structured content",
    exact: true,
  });
  await expect(
    explorer.getByText("Preview truncated", { exact: true }),
  ).toBeVisible();
  expect(
    fixture.reads.filter((value) => value.includes("/payloads/")),
  ).toHaveLength(0);
  const first = page.waitForRequest(
    (request) =>
      new URL(request.url()).pathname.includes("/payloads/") &&
      !new URL(request.url()).pathname.endsWith("/structure"),
  );
  await explorer
    .getByRole("button", { name: "Read exact value bytes", exact: true })
    .click();
  const url = new URL((await first).url());
  expect(url.searchParams.get("start")).toBe("1");
  expect(Number(url.searchParams.get("end")) - 1).toBeLessThanOrEqual(
    configured,
  );
  await expect(
    explorer.getByRole("button", { name: "Next range", exact: true }),
  ).toBeEnabled();
  const second = page.waitForRequest(
    (request) =>
      new URL(request.url()).pathname.includes("/payloads/") &&
      !new URL(request.url()).pathname.endsWith("/structure"),
  );
  await explorer
    .getByRole("button", { name: "Next range", exact: true })
    .click();
  const next = new URL((await second).url());
  expect(Number(next.searchParams.get("start"))).toBeGreaterThan(1);
  expect(
    Number(next.searchParams.get("end")) -
      Number(next.searchParams.get("start")),
  ).toBeLessThanOrEqual(Number(url.searchParams.get("end")) - 1);
  await expect(explorer.getByText("�", { exact: false })).toHaveCount(0);
});

test("an idle structured page clears its preview and releases its job at expiry", async ({
  page,
}) => {
  const row = message(0, "JSON source");
  if (row.content.kind !== "message") throw new Error("fixture");
  row.content.parts[0].payload.mediaType = "application/json";
  await installTraceFixture(page, [row]);
  let cancelled = 0;
  let expiry = 0;
  await page.route("**/api/v1/trace/payloads/*/structure?*", (route) => {
    expiry = Date.now() + 1500;
    return route.fulfill({
      status: 202,
      json: { ...status("building"), expiresAtMs: expiry },
    });
  });
  await page.route("**/api/v1/trace/structures/structure-one*", (route) => {
    if (route.request().method() === "DELETE") {
      cancelled++;
      return route.fulfill({ status: 204 });
    }
    const value = node(2, 0, "string");
    value.preview = {
      text: "Expires with its view",
      truncated: false,
      start: 0,
      end: 21,
    };
    return route.fulfill({
      json: {
        ...status("ready"),
        expiresAtMs: expiry,
        node: node(1, 0, "array", 1),
        items: [value],
        after: null,
      },
    });
  });
  await openTrace(page);
  await page.getByRole("button", { name: "Explore JSON", exact: true }).click();
  await expect(
    page.getByText("Expires with its view", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(
      "Structured exploration expired. Close and reopen it to build another index.",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(
    page.getByText("Expires with its view", { exact: true }),
  ).toHaveCount(0);
  await expect.poll(() => cancelled).toBe(1);
});
