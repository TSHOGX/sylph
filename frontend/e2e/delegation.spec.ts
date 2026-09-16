import { expect, test } from "./fixtures/test";
import {
  agentLink,
  bootstrap,
  CHILD_AGENT,
  MAIN_AGENT,
  traceView,
  installTraceFixture,
  message,
  openTrace,
  unavailableExecution,
} from "./fixtures/trace";

async function directory(page: import("@playwright/test").Page) {
  const state = await installTraceFixture(page, [
    message(0, "Parent retained answer"),
  ]);
  state.catalogue = Array.from({ length: 1000 }, (_, index) => ({
    agent: agentLink(
      index === 999 ? CHILD_AGENT : `opaque-child-${index}`,
      index === 999 ? "Needle child" : `Agent ${index}`,
    ),
    relation: "spawn",
    ownExecution: unavailableExecution,
    activations: { value: 1, scope: "Agent", complete: true },
    requiredInput: { value: 0, scope: "Agent", complete: true },
    coverage: [],
  }));
  state.mainHeader.ownExecution = {
    reference: "parent-activation",
    state: { kind: "terminated", outcome: "succeeded", reason: null },
    durations: [],
  };
  state.childHeader.ownExecution = {
    reference: "child-activation",
    state: { kind: "observed_active", lastObservedAtMs: 100 },
    durations: [],
  };
  return state;
}

test("the agent directory searches its complete scope without fetching child bodies", async ({
  page,
}) => {
  const state = await directory(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "Agent 0", exact: true }),
  ).toBeVisible();
  expect(
    state.reads.filter(
      (read) =>
        read.includes(`/agents/${CHILD_AGENT}?`) ||
        read.endsWith(`/agents/${CHILD_AGENT}`),
    ),
  ).toHaveLength(0);
  await page.getByLabel("Find agents").fill("Needle");
  await expect(
    page.getByRole("link", { name: "Needle child", exact: true }),
  ).toBeVisible();
  expect(state.searched).toContain("Needle");
});

test("a child uses its own identity and lifecycle in the same companion reader", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await directory(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await page.getByLabel("Find agents").fill("Needle");
  await page
    .getByRole("button", { name: "Open alongside", exact: true })
    .click();
  const inspector = page.getByLabel("Trace inspector", { exact: true });
  await expect(
    inspector.getByRole("heading", { name: /Unnamed agent/ }),
  ).toBeVisible();
  await expect(inspector.getByText(/Current history · Working/)).toBeVisible();
  await expect(page.getByText(/Current history · Completed/)).toBeVisible();
  await expect(
    inspector.getByText("The child owns this answer", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveCount(1);
  await inspector.getByRole("button", { name: "All agents" }).click();
  await expect(page.getByLabel("Find agents")).toBeVisible();
});

test("mobile child inspection fills the viewport and returns without changing control focus", async ({
  page,
}) => {
  await page.setViewportSize({ width: 375, height: 720 });
  await directory(page);
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await page.getByLabel("Find agents").fill("Needle");
  await page
    .getByRole("button", { name: "Open alongside", exact: true })
    .click();
  const inspector = page.getByRole("dialog", { name: "Trace inspector" });
  await expect(inspector).toBeVisible();
  expect((await inspector.boundingBox())!.height).toBeGreaterThan(700);
  await inspector.getByRole("button", { name: "Back to conversation" }).click();
  await expect(inspector).toHaveCount(0);
  await expect(
    page.getByText("Parent retained answer", { exact: true }),
  ).toBeVisible();
});

test("an independently opened embedded agent never gains its anchor session's controls", async ({
  page,
}) => {
  const state = await installTraceFixture(page);
  state.childHeader = bootstrap(CHILD_AGENT).header;
  await openTrace(page, CHILD_AGENT);
  await expect(
    page.getByText("Embedded child has no independent control route", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Send", exact: true }),
  ).toHaveCount(0);
  expect(state.commands).toHaveLength(0);
});

for (const entry of ["work", "delivery", "evidence"] as const) {
  test(`companion ${entry} inspection preserves its view and history without redirecting commands`, async ({
    page,
  }) => {
    if (entry === "evidence")
      await page.setViewportSize({ width: 375, height: 720 });
    const state = await directory(page);
    state.control = {
      kind: "available",
      target: {
        kind: "session",
        agent: MAIN_AGENT,
        sessionId: "fixture-session",
      },
      actions: ["send"],
      lease: null,
      affectedExecution: null,
    };
    state.childHeader.history = {
      ...state.childHeader.history,
      selection: "child-branch",
      label: "Child branch",
      isCurrent: false,
    };
    const child = message(1, "The child owns this answer", CHILD_AGENT);
    if (child.content.kind !== "message") throw new Error("Message fixture");
    child.content.delivery = {
      reference: "child-delivery",
      sender: { kind: "human", label: "You" },
      stages: [{ kind: "admitted", entry: null }],
      availability: { kind: "available" },
    };
    state.childRows = [
      child,
      {
        ...message(2, "", CHILD_AGENT),
        content: {
          kind: "work",
          owner: agentLink(CHILD_AGENT),
          operations: { value: 1, scope: "Child branch", complete: true },
          agents: { value: 0, scope: "Child branch", complete: true },
          assignments: { value: 0, scope: "Child branch", complete: true },
          failedOperations: { value: 0, scope: "Child branch", complete: true },
          activeResources: { value: 0, scope: "Child branch", complete: true },
          requiredInput: { value: 0, scope: "Child branch", complete: true },
          operationsAnchor: "child-work",
        },
      },
    ];
    const childView = traceView(11);
    const childReads: string[] = [];
    await page.route(
      (url) => url.pathname === `/api/v1/trace/agents/${CHILD_AGENT}`,
      (route) => {
        childReads.push(route.request().url());
        const result = bootstrap(CHILD_AGENT, state.childRows, childView);
        result.header = state.childHeader;
        return route.fulfill({ json: result });
      },
    );
    const reference =
      entry === "work"
        ? "child-work"
        : entry === "delivery"
          ? "child-delivery"
          : child.anchor;
    const detail = {
      view: childView,
      reference,
      title: `Child ${entry} detail`,
      owner: agentLink(CHILD_AGENT),
      execution: null,
      availability: { kind: "available" as const },
      content: {
        kind: "generic" as const,
        label: "Child detail",
        payloads: [],
      },
      relations: [
        {
          target: "nested-child-detail",
          label: "Nested child detail",
          agent: null,
        },
      ],
      relationsCursor: null,
      coverage: [],
    };
    state.details.set(reference, detail);
    state.details.set("nested-child-detail", {
      ...detail,
      reference: "nested-child-detail",
      title: "Nested retained detail",
      relations: [],
    });
    await openTrace(page);
    await page.getByRole("button", { name: "Agents", exact: true }).click();
    await page.getByLabel("Find agents").fill("Needle");
    await page
      .getByRole("button", { name: "Open alongside", exact: true })
      .click();
    const inspector = page.getByLabel("Trace inspector", { exact: true });
    const action =
      entry === "work"
        ? inspector.getByRole("button", { name: /^Work ·/ })
        : entry === "delivery"
          ? inspector.getByRole("button", { name: /^Delivery:/ })
          : inspector
              .getByRole("button", {
                name: "Evidence and details",
                exact: true,
              })
              .first();
    await action.click();
    await expect(
      inspector.getByRole("heading", {
        name: `Child ${entry} detail`,
        exact: true,
      }),
    ).toBeVisible();
    const scoped = (target: string) =>
      state.reads.some((read) => {
        const url = new URL(read.slice(4), "http://fixture");
        return (
          url.pathname === `/api/v1/trace/objects/${target}` &&
          url.searchParams.get("at_snapshot") === childView.token &&
          url.searchParams.get("selection") === "child-branch"
        );
      });
    expect(scoped(reference)).toBe(true);
    if (entry === "work")
      await inspector
        .getByRole("button", { name: "Pin detail", exact: true })
        .click();
    await inspector
      .getByRole("button", { name: "Nested child detail", exact: true })
      .click();
    await expect(
      inspector.getByRole("heading", {
        name: "Nested retained detail",
        exact: true,
      }),
    ).toBeVisible();
    expect(scoped("nested-child-detail")).toBe(true);
    await inspector
      .getByRole("button", { name: "Back to previous detail", exact: true })
      .click();
    await expect(
      inspector.getByRole("heading", {
        name: `Child ${entry} detail`,
        exact: true,
      }),
    ).toBeVisible();
    await inspector
      .getByRole("button", { name: "Back to previous detail", exact: true })
      .click();
    await expect(
      inspector.getByText("The child owns this answer", { exact: true }),
    ).toBeVisible();
    const restoredChild = new URL(childReads.at(-1)!);
    expect(restoredChild.searchParams.get("at_snapshot")).toBe(childView.token);
    expect(restoredChild.searchParams.get("selection")).toBe("child-branch");
    if (entry === "work") {
      await inspector
        .getByRole("button", { name: "Detail 1", exact: true })
        .click();
      await expect(
        inspector.getByRole("heading", {
          name: "Child work detail",
          exact: true,
        }),
      ).toBeVisible();
      expect(scoped(reference)).toBe(true);
    }
    expect(
      state.reads.some(
        (read) => read === `DELETE /api/v1/trace/views/${childView.token}`,
      ),
    ).toBe(false);

    await inspector
      .getByRole("button", {
        name: /^(Close|Back to conversation)$/,
        exact: true,
      })
      .click();
    await page
      .getByLabel("Message", { exact: true })
      .fill("Still addressed to the main agent");
    await page.getByRole("button", { name: "Send", exact: true }).click();
    await expect
      .poll(() => state.commands[0]?.target)
      .toEqual({
        kind: "session",
        agent: MAIN_AGENT,
        sessionId: "fixture-session",
      });
  });
}

test("returning from a companion restores the filtered directory page and original row position", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const state = await directory(page);
  state.catalogue = Array.from({ length: 120 }, (_, index) => ({
    agent: agentLink(
      index === 55 ? CHILD_AGENT : `directory-child-${index}`,
      `Branch agent ${index}`,
    ),
    relation: "spawn",
    ownExecution: unavailableExecution,
    activations: { value: 1, scope: "Agent", complete: true },
    requiredInput: { value: 0, scope: "Agent", complete: true },
    coverage: [],
  }));
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await page.getByLabel("Find agents").fill("Branch");
  const inspector = page.getByLabel("Trace inspector", { exact: true });
  await inspector
    .getByRole("combobox", { name: /^Show/ })
    .selectOption("unknown");
  await inspector
    .getByRole("button", { name: "Next page", exact: true })
    .click();
  const row = inspector.getByRole("listitem").filter({
    has: page.getByRole("link", { name: "Branch agent 55", exact: true }),
  });
  const open = row.getByRole("button", { name: "Open alongside", exact: true });
  await open.scrollIntoViewIfNeeded();
  await open.focus();
  const before = (await row.boundingBox())!.y;
  await open.click();
  await expect(
    inspector.getByText("The child owns this answer", { exact: true }),
  ).toBeVisible();
  const readsBeforeReturn = state.reads.length;
  await inspector.getByRole("button", { name: "All agents" }).click();
  await expect(page.getByLabel("Find agents")).toHaveValue("Branch");
  await expect(inspector.getByRole("combobox", { name: /^Show/ })).toHaveValue(
    "unknown",
  );
  await expect(row).toBeVisible();
  await expect(open).toBeFocused();
  await expect
    .poll(async () => Math.abs((await row.boundingBox())!.y - before))
    .toBeLessThanOrEqual(2);
  const query = state.reads
    .slice(readsBeforeReturn)
    .find((read) => read.includes("/relations?"));
  expect(query).toBeDefined();
  const params = new URL(query!.slice(4), "http://fixture").searchParams;
  expect(params.get("q")).toBe("Branch");
  expect(params.get("filter")).toBe("unknown");
  expect(params.get("after")).toBe("40");
});

test("a pinned companion detail stays in the shared observation scope after its reader closes", async ({
  page,
}) => {
  const state = await directory(page);
  const child = state.childRows[0];
  state.details.set(child.anchor, {
    view: traceView(),
    reference: child.anchor,
    title: "Pinned child evidence",
    owner: agentLink(CHILD_AGENT),
    execution: null,
    availability: { kind: "available" },
    content: { kind: "generic", label: "Child evidence", payloads: [] },
    relations: [],
    relationsCursor: null,
    coverage: [],
  });
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await page.getByLabel("Find agents").fill("Needle");
  await page
    .getByRole("button", { name: "Open alongside", exact: true })
    .click();
  const inspector = page.getByLabel("Trace inspector", { exact: true });
  await inspector
    .getByRole("button", { name: "Evidence and details", exact: true })
    .click();
  await expect(
    inspector.getByRole("heading", {
      name: "Pinned child evidence",
      exact: true,
    }),
  ).toBeVisible();
  await inspector
    .getByRole("button", { name: "Pin detail", exact: true })
    .click();
  await expect
    .poll(() => {
      const read = state.reads
        .filter((read) => read.includes("/changes?"))
        .at(-1);
      return read
        ? JSON.parse(
            new URL(read.slice(4), "http://fixture").searchParams.get(
              "interests",
            )!,
          )
        : [];
    })
    .toEqual(expect.arrayContaining([MAIN_AGENT, CHILD_AGENT]));
  await page.route("**/api/v1/trace/changes?*", (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ kind: "access_changed", authorizationScope: "child-revoked" })}\n\n`,
    }),
  );
  await expect(
    inspector.getByText("Pinned child evidence", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText(/Access changed\. Open this conversation/),
  ).toBeVisible();
  expect(state.commands).toHaveLength(0);
});
