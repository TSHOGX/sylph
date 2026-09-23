import { expect, test } from "./fixtures/test";
import {
  agentLink,
  CHILD_AGENT,
  installTraceFixture,
  MAIN_AGENT,
  message,
  openTrace,
  traceView,
} from "./fixtures/trace";

test("a fork origin opens an independent reader and Back preserves the fork draft", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Fork-owned conversation"),
  ]);
  state.mainHeader.agent = agentLink(MAIN_AGENT, "Fork reader");
  state.mainHeader.origin = agentLink(CHILD_AGENT, "Original reader");
  state.mainHeader.parent = null;
  state.childHeader.agent = agentLink(CHILD_AGENT, "Original reader");
  state.childHeader.parent = null;
  state.childRows = [message(1, "Original retained conversation", CHILD_AGENT)];
  await openTrace(page);
  await page
    .getByLabel("Unsent draft", { exact: true })
    .fill("Draft belongs to the fork");
  await expect(page.getByRole("link", { name: /^Parent:/ })).toHaveCount(0);
  await page
    .getByRole("link", { name: "Origin: Original reader", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Original reader", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Original retained conversation", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
    "",
  );
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "Fork reader", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
    "Draft belongs to the fork",
  );
  expect(state.commands).toHaveLength(0);
});

test("copy lineage inspection keeps its view and resets only the cross-agent history scope", async ({
  page,
}) => {
  const row = message(0, "Child-retained copy");
  const state = await installTraceFixture(page, [row]);
  state.mainHeader.history = {
    ...state.mainHeader.history,
    selection: "child-history",
    selectionKnown: true,
    isCurrent: true,
  };
  const selectedHistory = state.mainHeader.history.selection;
  const view = traceView();
  state.details.set(row.anchor, {
    view,
    reference: row.anchor,
    title: "Copied entry",
    owner: agentLink(MAIN_AGENT),
    execution: null,
    availability: { kind: "available" },
    content: { kind: "generic", label: "History entry", payloads: [] },
    relations: [
      {
        target: "original-entry",
        containingAgent: CHILD_AGENT,
        label: "Copy lineage",
        agent: null,
      },
    ],
    relationsCursor: null,
    coverage: [],
  });
  const requests: URL[] = [];
  await page.route(
    "**/api/v1/trace/objects/original-entry?*",
    async (route) => {
      const url = new URL(route.request().url());
      requests.push(url);
      if (url.searchParams.has("selection"))
        return route.fulfill({
          status: 400,
          json: {
            error: {
              code: "invalid_selection",
              message: "The child history does not select the original agent",
            },
          },
        });
      return route.fulfill({
        json: {
          view,
          reference: "original-entry",
          title: "Original source entry",
          owner: agentLink(CHILD_AGENT),
          execution: null,
          availability: { kind: "available" },
          content: {
            kind: "generic",
            label: "Original history entry",
            payloads: [],
          },
          relations: [],
          relationsCursor: null,
          coverage: [],
        },
      });
    },
  );
  await openTrace(page);
  await page
    .getByLabel("Unsent draft", { exact: true })
    .fill("Keep the child draft");
  await page
    .getByRole("button", { name: "Evidence and details", exact: true })
    .click();
  await page.getByRole("button", { name: "Copy lineage", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Original source entry", exact: true }),
  ).toBeVisible();
  expect(requests).toHaveLength(1);
  expect(requests[0].searchParams.get("at_snapshot")).toBe(view.token);
  expect(requests[0].searchParams.get("selection")).toBeNull();
  await page
    .getByRole("button", { name: "Back to previous detail", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Copied entry", exact: true }),
  ).toBeVisible();
  expect(
    state.reads.some(
      (read) =>
        read.includes(`/objects/${row.anchor}?`) &&
        read.includes(`selection=${selectedHistory}`),
    ),
  ).toBe(true);
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
    "Keep the child draft",
  );
  expect(state.commands).toHaveLength(0);
});
