import { expect, test } from "./fixtures/test";
import type { ReadingUnit } from "../lib/trace-types";
import {
  agentLink,
  CHILD_AGENT,
  installTraceFixture,
  message,
  openTrace,
  traceView,
} from "./fixtures/trace";

test.use({ hasTouch: true });
const unknown = { value: null, complete: false, scope: "Unobserved" };
function rows(): ReadingUnit[] {
  const owner = agentLink(CHILD_AGENT, "Original reader");
  const known = message(0, "Copied answer with a retained origin");
  known.entryContext = { kind: "inherited", originalOwner: owner };
  if (known.content.kind === "message")
    known.content.author = {
      kind: "agent",
      agent: owner,
      label: "Original reader",
    };
  const unavailable = message(1, "Copied answer whose origin is unavailable");
  unavailable.entryContext = { kind: "inherited", originalOwner: null };
  if (unavailable.content.kind === "message")
    unavailable.content.author = {
      kind: "agent",
      agent: null,
      label: "Agent · identity unavailable",
    };
  const work = message(2);
  work.entryContext = { kind: "unresolved" };
  work.content = {
    kind: "work",
    owner: null,
    operations: { value: 1, complete: true, scope: "This invocation" },
    agents: unknown,
    assignments: unknown,
    failedOperations: unknown,
    activeResources: unknown,
    requiredInput: unknown,
    operationsAnchor: "unresolved-operation",
  };
  const checkpoint = message(3);
  checkpoint.entryContext = { kind: "inherited", originalOwner: null };
  checkpoint.content = {
    kind: "context_boundary",
    checkpoint: "copied-checkpoint",
    label: "Retained context change",
    summary: null,
  };
  const opaque = message(4);
  opaque.entryContext = { kind: "inherited", originalOwner: null };
  opaque.content = {
    kind: "unknown",
    label: "Opaque copied entry",
    reason: "Retained source content",
    payloads: [],
  };
  const incoming = message(5, "A separate incoming message");
  if (incoming.content.kind === "message")
    incoming.content.author = {
      kind: "agent",
      agent: owner,
      label: "Original reader",
    };
  return [known, unavailable, work, checkpoint, opaque, incoming];
}
for (const width of [1280, 390]) {
  test(`entry provenance survives missing origins and origin inspection preserves the reader at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const values = rows(),
      fixture = await installTraceFixture(page, values);
    fixture.mainHeader.history.selection = "recipient-selection";
    fixture.childHeader.agent = agentLink(CHILD_AGENT, "Original reader");
    fixture.childHeader.parent = null;
    fixture.childRows = [message(10, "Source-owned answer", CHILD_AGENT)];
    const originReads: URL[] = [];
    await page.route(`**/api/v1/trace/agents/${CHILD_AGENT}?*`, (route) => {
      originReads.push(new URL(route.request().url()));
      return route.fallback();
    });
    await openTrace(page);
    const article = (index: number) =>
      page.locator(`[data-trace-address="${values[index].address}"]`);
    for (const index of [0, 1, 3, 4])
      await expect(
        article(index).getByText("Inherited context", { exact: true }),
      ).toBeVisible();
    for (const index of [1, 3, 4]) {
      await expect(
        article(index).getByText("Origin unavailable", { exact: true }),
      ).toBeVisible();
      await expect(
        article(index).getByRole("button", { name: /^Origin:/ }),
      ).toHaveCount(0);
    }
    await expect(
      article(2).getByText("Context provenance unresolved", { exact: true }),
    ).toBeVisible();
    await expect(
      article(2).getByText("Inherited context", { exact: true }),
    ).toHaveCount(0);
    await expect(
      article(5).getByLabel("Entry context", { exact: true }),
    ).toHaveCount(0);
    expect(originReads).toHaveLength(0);
    await page
      .getByLabel("Unsent draft", { exact: true })
      .fill("Keep the recipient draft");
    await article(0)
      .getByRole("button", { name: "Origin: Original reader", exact: true })
      .click();
    await expect(
      page.getByText("Source-owned answer", { exact: true }),
    ).toBeVisible();
    expect(originReads.length).toBeGreaterThan(0);
    expect(originReads[0].searchParams.get("at_snapshot")).toBe(
      traceView().token,
    );
    expect(originReads[0].searchParams.get("selection")).toBeNull();
    expect(fixture.commands).toHaveLength(0);
    await page
      .getByRole("button", {
        name: width < 768 ? "Back to conversation" : "Close",
        exact: true,
      })
      .click();
    await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
      "Keep the recipient draft",
    );
    await article(1).scrollIntoViewIfNeeded();
    await testInfo.attach(`entry-context-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
  });
}
