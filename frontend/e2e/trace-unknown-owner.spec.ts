import { expect, test } from "./fixtures/test";
import type { ReadingUnit } from "../lib/trace-types";
import {
  installTraceFixture,
  openTrace,
  payload,
  traceView,
  unavailableExecution,
} from "./fixtures/trace";

for (const width of [1280, 390]) {
  test(`an invocation with unknown ownership opens retained input at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const count = { value: 1, complete: true, scope: "this invocation" };
    const unknown = { value: null, complete: false, scope: "unobserved" };
    const operation = "unowned-operation";
    const row: ReadingUnit = {
      address: "unowned-history-slot",
      anchor: "retained-history-entry",
      orderBasis: "native_predecessor",
      sourceSurface: "Retained history",
      activation: null,
      content: {
        kind: "work",
        owner: null,
        operations: count,
        agents: unknown,
        assignments: unknown,
        failedOperations: unknown,
        activeResources: unknown,
        requiredInput: unknown,
        operationsAnchor: operation,
      },
    };
    const fixture = await installTraceFixture(page, [row]);
    const body = "Retained operation arguments.";
    fixture.bodies.set("unowned-input", body);
    fixture.details.set(operation, {
      view: traceView(),
      reference: operation,
      title: "Historical invocation",
      owner: null,
      execution: unavailableExecution,
      availability: { kind: "available" },
      content: {
        kind: "operation",
        category: { kind: "tool", namespace: "fixture", name: "read" },
        input: [
          {
            ...payload("unowned-input", operation),
            representation: "runtime",
            totalBytes: body.length,
            availableRanges: [{ start: 0, endExclusive: body.length }],
          },
        ],
        results: [],
        resultsCursor: null,
        resources: [],
        resourcesCursor: null,
      },
      relations: [],
      relationsCursor: null,
      coverage: [],
    });
    await openTrace(page);
    await page
      .getByLabel("Unsent draft", { exact: true })
      .fill("Keep this draft");
    await expect(
      page.getByText("Owner: unknown", { exact: true }),
    ).toBeVisible();
    expect(
      fixture.reads.filter((read) => read.includes("/payloads/")),
    ).toHaveLength(0);
    await testInfo.attach(`unknown-owner-work-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
    await page.getByRole("button", { name: /^Work ·/ }).click();
    await expect(
      page.getByRole("heading", { name: "Historical invocation", exact: true }),
    ).toBeVisible();
    const inspector = page.getByRole(
      width < 768 ? "dialog" : "complementary",
      { name: "Trace inspector" },
    );
    await expect(
      inspector.getByRole("link", { name: /^Owner:/ }),
    ).toHaveCount(0);
    await expect(
      inspector.getByText("Execution status unavailable", { exact: true }),
    ).toBeVisible();
    await inspector
      .getByRole("button", { name: /^Read runtime output$/i })
      .click();
    await expect(inspector.getByText(body, { exact: true })).toBeVisible();
    await testInfo.attach(`unknown-owner-input-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
    expect(
      fixture.reads.some((read) => read.includes(`/objects/${operation}?`)),
    ).toBe(true);
    await inspector
      .getByRole("button", {
        name: width < 768 ? "Back to conversation" : "Close",
        exact: true,
      })
      .click();
    await expect(
      page.getByLabel("Unsent draft", { exact: true }),
    ).toHaveValue("Keep this draft");
    expect(fixture.commands).toHaveLength(0);
  });
}
