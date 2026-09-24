import { expect, test } from "./fixtures/test";
import {
  installTraceFixture,
  message,
  openTrace,
  MAIN_AGENT,
  payload,
  traceView,
  agentLink,
  resultCatalogue,
} from "./fixtures/trace";

test("unknown authorship stays readable without becoming human input or an invented activation", async ({
  page,
}) => {
  const row = message(0, "Source did not identify this author");
  if (row.content.kind !== "message") throw new Error("message fixture");
  row.content.author = { kind: "unknown" };
  await installTraceFixture(page, [row]);
  await openTrace(page);
  await expect(
    page.getByRole("heading", { name: "Authorship unresolved" }),
  ).toBeVisible();
  await expect(
    page.getByText("Source did not identify this author", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Completed", exact: true }),
  ).toHaveCount(0);
});

test("equal text in separate deliveries keeps separate reading occurrences", async ({
  page,
}) => {
  await installTraceFixture(page, [
    message(0, "Equal retained text"),
    message(1, "Equal retained text"),
  ]);
  await openTrace(page);
  await expect(
    page.getByText("Equal retained text", { exact: true }),
  ).toHaveCount(2);
  await expect(page.locator("[data-trace-address]")).toHaveCount(2);
});

test("runtime and caller results remain independently labelled and lazily read", async ({
  page,
}) => {
  const row = message(0, "Inspect this operation");
  const state = await installTraceFixture(page, [row]);
  const runtime = {
    ...payload("runtime", row.anchor),
    representation: "runtime" as const,
  };
  const returned = {
    ...payload("returned", row.anchor),
    representation: "caller_result" as const,
  };
  state.details.set(row.anchor, {
    view: traceView(),
    reference: row.anchor,
    title: "Retained operation",
    owner: agentLink(),
    execution: null,
    availability: { kind: "available" },
    content: {
      kind: "operation",
      category: { kind: "tool", namespace: null, name: "shell" },
      input: [],
      resultRepresentations: resultCatalogue(["runtime", "caller_result"]),
      resources: [],
      resourcesCursor: null,
    },
    relations: [],
    relationsCursor: null,
    coverage: [],
  });
  state.results.set(row.anchor, [
    {
      reference: "result-runtime",
      disposition: "progress",
      representation: "runtime",
      label: "Runtime observation",
      payloads: [runtime],
      availability: { kind: "available" },
    },
    {
      reference: "result-returned",
      disposition: "final_return",
      representation: "caller_result",
      label: "Returned value",
      payloads: [returned],
      availability: { kind: "available" },
    },
  ]);
  state.bodies.set("runtime", "Complete runtime bytes");
  state.bodies.set("returned", "Shortened caller result");
  await openTrace(page);
  await page.getByRole("button", { name: "Evidence and details" }).click();
  await expect(page.getByLabel("Representation", { exact: true })).toHaveValue(
    "caller_result",
  );
  await page
    .getByRole("button", { name: "Read returned to caller", exact: true })
    .click();
  await expect(
    page.getByText("Shortened caller result", { exact: true }),
  ).toBeVisible();
  expect(
    state.reads.filter((read) => read.includes("/payloads/runtime")),
  ).toHaveLength(0);
  await page
    .getByLabel("Representation", { exact: true })
    .selectOption("runtime");
  await page
    .getByRole("button", { name: "Read runtime output", exact: true })
    .click();
  await expect(
    page.getByText("Complete runtime bytes", { exact: true }),
  ).toBeVisible();
});

test("search reaches unloaded content and opens the retained search view", async ({
  page,
}) => {
  const state = await installTraceFixture(
    page,
    Array.from({ length: 120 }, (_, index) => message(index)),
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Find in this agent" }).click();
  await page.getByLabel("Find in Main agent").fill("unloaded");
  await page.getByRole("button", { name: "Search retained history" }).click();
  await expect(
    page.getByText("A retained hit outside the visible window", {
      exact: true,
    }),
  ).toBeVisible();
  await page
    .getByRole("link", { name: "Open Search checkpoint around this item" })
    .click();
  await expect(page).toHaveURL(new RegExp(`${MAIN_AGENT}.*anchor=`));
  await expect
    .poll(() =>
      state.reads.some(
        (read) =>
          read.includes("around=opaque-main-agent-anchor-3") &&
          read.includes("at_snapshot=view-1"),
      ),
    )
    .toBe(true);
});

test("removing payload access clears protected reading state", async ({
  page,
}) => {
  const row = message(0, "Protected preview");
  if (row.content.kind !== "message") throw new Error("message fixture");
  row.content.parts[0].previewTruncated = true;
  const state = await installTraceFixture(page, [row]);
  await openTrace(page);
  state.failPayload = true;
  await page
    .getByRole("button", {
      name: "Read recorded conversation result",
      exact: true,
    })
    .click();
  await expect(
    page.getByText("Protected preview", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText(/Access changed\. Open this conversation/),
  ).toBeVisible();
});
