import { expect, test } from "./fixtures/test";
import {
  agentLink,
  installTraceFixture,
  MAIN_AGENT,
  message,
  openTrace,
  tracePage,
  traceView,
} from "./fixtures/trace";

for (const width of [1280, 390]) {
  test(`search scope resets its cursor and archive hits keep their checkpoint without sending at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const rows = [
      message(0, "Local answer"),
      message(1, "Copied retained answer"),
    ];
    rows[1].entryContext = { kind: "inherited", originalOwner: null };
    const fixture = await installTraceFixture(page, rows);
    fixture.mainHeader.history.selection = "current-branch";
    fixture.control = {
      kind: "available",
      target: {
        kind: "session",
        agent: MAIN_AGENT,
        sessionId: "fixture-session",
      },
      actions: ["send"],
      lease: "control-lease",
      affectedExecution: null,
    };
    const reads: URL[] = [];
    await page.route("**/api/v1/trace/search?*", (route) => {
      const url = new URL(route.request().url());
      reads.push(url);
      const mode = url.searchParams.get("mode"),
        retained = mode === "retained_archive",
        selected = mode === "selected_context";
      const view = traceView(retained ? 12 : 11);
      const hit = {
        anchor: rows[retained ? 1 : 0].anchor,
        agent: agentLink(),
        history: {
          archive: !selected,
          selection: selected ? "current-branch" : null,
          label: retained ? "Retained archive" : "Scoped result",
          isCurrent: false,
          selectionKnown: true,
          availability: { kind: "available" },
        },
        excerpt: retained ? "Copied retained answer" : "Local scoped result",
        payload: null,
      };
      const result = tracePage([hit], view, 0, selected ? 2 : 1);
      if (selected && !url.searchParams.has("cursor")) {
        result.afterCursor = "next-selected";
        result.hasAfter = true;
      }
      return route.fulfill({ json: result });
    });
    await openTrace(page);
    await page
      .getByLabel("Message", { exact: true })
      .fill("Keep the search draft");
    await page
      .getByRole("button", { name: "Find in this agent", exact: true })
      .click();
    await page.getByLabel("Find in Main agent", { exact: true }).fill("needle");
    await page
      .getByRole("button", { name: "Search retained history", exact: true })
      .click();
    await expect(
      page.getByText("Local scoped result", { exact: true }),
    ).toBeVisible();
    expect(reads[0].searchParams.get("mode")).toBe("selected_context");
    expect(reads[0].searchParams.get("selection")).toBe("current-branch");
    await page.getByRole("button", { name: "Next page", exact: true }).click();
    await expect.poll(() => reads.length).toBe(2);
    expect(reads[1].searchParams.get("cursor")).toBe("next-selected");
    expect(reads[1].searchParams.get("at_snapshot")).toBe("view-11");
    await page
      .getByRole("button", { name: "Search scope", exact: true })
      .click();
    await page
      .getByRole("menuitemradio", {
        name: "Locally authored messages",
        exact: true,
      })
      .click();
    await expect.poll(() => reads.length).toBe(3);
    expect(reads[2].searchParams.get("mode")).toBe("authored");
    for (const key of ["cursor", "at_snapshot", "selection"])
      expect(reads[2].searchParams.get(key)).toBeNull();
    await page
      .getByRole("button", { name: "Search scope", exact: true })
      .click();
    await page
      .getByRole("menuitemradio", { name: "Retained history", exact: true })
      .click();
    await expect.poll(() => reads.length).toBe(4);
    expect(reads[3].searchParams.get("mode")).toBe("retained_archive");
    for (const key of ["cursor", "at_snapshot", "selection"])
      expect(reads[3].searchParams.get(key)).toBeNull();
    await page
      .getByRole("link", {
        name: "Open Retained archive around this item",
        exact: true,
      })
      .click();
    await expect(page).toHaveURL(/history=archive/);
    await expect(
      page.getByRole("link", {
        name: "Return to current conversation to send",
        exact: true,
      }),
    ).toBeVisible();
    await expect(page.getByLabel("Message", { exact: true })).toHaveCount(0);
    await expect
      .poll(() =>
        fixture.reads.some(
          (read) =>
            read.includes("archive=true") &&
            read.includes("at_snapshot=view-12") &&
            !read.includes("selection="),
        ),
      )
      .toBe(true);
    await page.keyboard.press("ControlOrMeta+Enter");
    expect(fixture.commands).toHaveLength(0);
    await testInfo.attach(`archive-search-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
    await page
      .getByRole("link", {
        name: "Return to current conversation to send",
        exact: true,
      })
      .click();
    await expect(page.getByLabel("Message", { exact: true })).toHaveValue(
      "Keep the search draft",
    );
    if (width < 768) {
      await page.getByRole("button", { name: "Details", exact: true }).click();
      await expect(
        page.getByRole("dialog", { name: "Trace inspector", exact: true }),
      ).toBeVisible();
      await page.keyboard.press("ControlOrMeta+k");
      const search = page.getByRole("dialog", {
        name: "Find conversations",
        exact: true,
      });
      await search
        .getByRole("textbox", {
          name: "Search retained conversations",
          exact: true,
        })
        .fill("needle");
      await search.getByRole("button", { name: "Search", exact: true }).click();
      await search
        .getByRole("link", { name: "Open this retained result", exact: true })
        .click();
      await expect(
        page.getByRole("dialog", { name: "Trace inspector", exact: true }),
      ).toHaveCount(0);
      await expect(
        page.getByRole("link", {
          name: "Return to current conversation to send",
          exact: true,
        }),
      ).toBeVisible();
      expect(fixture.commands).toHaveLength(0);
    }
  });
}

test("changing search scope cancels the pending request without mixing its results", async ({
  page,
}) => {
  await installTraceFixture(page, [message(0)]);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let oldStarted = false;
  let cancelled = false;
  page.on("requestfailed", (request) => {
    const url = new URL(request.url());
    if (
      url.pathname.endsWith("/trace/search") &&
      url.searchParams.get("mode") === "retained_archive"
    )
      cancelled = true;
  });
  await page.route("**/api/v1/trace/search?*", async (route) => {
    const mode = new URL(route.request().url()).searchParams.get("mode");
    if (mode === "retained_archive") {
      oldStarted = true;
      await held;
    }
    const hit = {
      anchor: "anchor",
      agent: agentLink(),
      history: {
        archive: true,
        selection: null,
        label: "Retained archive",
        isCurrent: false,
        selectionKnown: true,
        availability: { kind: "available" },
      },
      excerpt:
        mode === "authored" ? "New authored result" : "Stale retained result",
      payload: null,
    };
    await route
      .fulfill({
        json: tracePage([hit], traceView(mode === "authored" ? 12 : 99)),
      })
      .catch(() => {});
  });
  try {
    await openTrace(page);
    await page
      .getByRole("button", { name: "Find in this agent", exact: true })
      .click();
    await page.getByLabel("Find in Main agent", { exact: true }).fill("needle");
    await page
      .getByRole("button", { name: "Search retained history", exact: true })
      .click();
    await expect.poll(() => oldStarted).toBe(true);
    await page
      .getByRole("button", { name: "Search scope", exact: true })
      .click();
    await page
      .getByRole("menuitemradio", {
        name: "Locally authored messages",
        exact: true,
      })
      .click();
    await expect(
      page.getByText("New authored result", { exact: true }),
    ).toBeVisible();
    await expect.poll(() => cancelled).toBe(true);
    release();
    await expect(
      page.getByText("Stale retained result", { exact: true }),
    ).toHaveCount(0);
  } finally {
    release();
  }
});

for (const selection of ["historical-branch", "current-at-checkpoint"]) {
  test(`explicit selection ${selection} offers return-to-current instead of dispatching through live controls`, async ({
    page,
  }) => {
    const fixture = await installTraceFixture(page, [message(0)]);
    fixture.control = {
      kind: "available",
      target: {
        kind: "session",
        agent: MAIN_AGENT,
        sessionId: "fixture-session",
      },
      actions: ["send"],
      lease: "control-lease",
      affectedExecution: null,
    };
    fixture.mainHeader.history.selection = "current-at-checkpoint";
    await page.goto(`/agents/${MAIN_AGENT}?history=${selection}`);
    await expect(
      page.getByRole("link", {
        name: "Return to current conversation to send",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Send", exact: true }),
    ).toHaveCount(0);
    await page.keyboard.press("ControlOrMeta+Enter");
    expect(fixture.commands).toHaveLength(0);
  });
}

test("unknown native selection does not disable an authorized live conversation", async ({
  page,
}) => {
  const fixture = await installTraceFixture(page, [message(0)]);
  fixture.mainHeader.history = {
    archive: false,
    selection: null,
    label: "Retained history · selection unknown",
    isCurrent: false,
    selectionKnown: false,
    availability: { kind: "partial", reason: "Selected membership is unknown" },
  };
  fixture.control = {
    kind: "available",
    target: {
      kind: "session",
      agent: MAIN_AGENT,
      sessionId: "fixture-session",
    },
    actions: ["send"],
    lease: "control-lease",
    affectedExecution: null,
  };
  await openTrace(page);
  await page
    .getByLabel("Message", { exact: true })
    .fill("Continue current work");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect.poll(() => fixture.commands.length).toBe(1);
  expect(fixture.commands[0]).toMatchObject({
    action: "send",
    text: "Continue current work",
    target: fixture.control.target,
  });
});
