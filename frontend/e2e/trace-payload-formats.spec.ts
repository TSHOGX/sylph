import { expect, test } from "./fixtures/test";
import {
  agentLink,
  installTraceFixture,
  message,
  openTrace,
  payload,
  resultCatalogue,
  traceView,
} from "./fixtures/trace";
import type { PayloadAccess } from "../lib/trace-types";

for (const width of [1280, 390]) {
  test(`equal retained bytes preserve text, JSON and unknown formats at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const row = message(0, "Inspect format evidence");
    const fixture = await installTraceFixture(page, [row]);
    const inputs: PayloadAccess[] = [
      { mediaType: "text/plain", encoding: "utf8" },
      { mediaType: "application/json", encoding: "utf8" },
      { mediaType: null, encoding: null },
    ].map((format, index) => ({
      ...payload("same-bytes", row.anchor),
      ...format,
      encoding: format.encoding as "utf8" | null,
      bodyRevision: `use-${index}`,
      representation: "runtime",
      totalBytes: 2,
      availableRanges: [{ start: 0, endExclusive: 2 }],
    }));
    fixture.bodies.set("same-bytes", "{}");
    fixture.details.set(row.anchor, {
      view: traceView(),
      reference: row.anchor,
      title: "Recorded input formats",
      owner: agentLink(),
      execution: null,
      availability: { kind: "available" },
      content: {
        kind: "operation",
        category: { kind: "wait" },
        input: inputs,
        resultRepresentations: resultCatalogue(),
        resources: [],
        resourcesCursor: null,
      },
      relations: [],
      relationsCursor: null,
      coverage: [],
    });
    await openTrace(page);
    await page
      .getByRole("button", { name: "Evidence and details", exact: true })
      .click();
    const inspector = page.getByRole(width < 768 ? "dialog" : "complementary", {
      name: "Trace inspector",
    });
    expect(
      fixture.reads.filter((read) => read.includes("/payloads/")),
    ).toHaveLength(0);
    for (const input of inputs) {
      await inspector
        .getByRole("button", { name: "Read input", exact: true })
        .first()
        .click();
      await expect
        .poll(
          () =>
            fixture.reads.filter((read) => read.includes("/payloads/")).length,
        )
        .toBe(inputs.indexOf(input) + 1);
    }
    await expect(inspector.getByText("{}", { exact: true })).toHaveCount(2);
    await expect(inspector.getByText(/Input · text\/plain/)).toBeVisible();
    await expect(
      inspector.getByText(/Input · application\/json/),
    ).toBeVisible();
    await expect(
      inspector.getByText(/Input · Media type unknown/),
    ).toBeVisible();
    await expect(inspector.getByText(/Binary content/)).toBeVisible();
    await expect(
      inspector.getByRole("combobox", { name: "Representation", exact: true }),
    ).toHaveCount(0);
    const requested = fixture.reads
      .filter((read) => read.includes("/payloads/"))
      .map((read) =>
        new URL(read.slice(4), "http://fixture").searchParams.get("revision"),
      );
    expect(requested).toEqual(inputs.map((input) => input.bodyRevision));
    expect(fixture.commands).toHaveLength(0);
    await testInfo.attach(`payload-formats-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
  });
}
