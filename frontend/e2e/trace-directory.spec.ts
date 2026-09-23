import { expect, test, type Page } from "./fixtures/test";
import {
  agentLink,
  installTraceFixture,
  MAIN_AGENT,
  message,
  openTrace,
  traceLimits,
  tracePage,
  traceView,
  unavailableExecution,
} from "./fixtures/trace";

async function setup(page: Page, holdInitialCount = false) {
  let releaseCount!: () => void;
  let enteredCount!: () => void;
  const countGate = new Promise<void>((resolve) => {
    releaseCount = resolve;
  });
  const countStarted = new Promise<void>((resolve) => {
    enteredCount = resolve;
  });
  const state = await installTraceFixture(page, [
    message(0, "Root conversation"),
  ]);
  state.mainHeader.agent = agentLink(MAIN_AGENT, "Focused root");
  const catalogue = Array.from(
    { length: traceLimits.maxItems * 3 + 7 },
    (_, index) => ({
      agent: agentLink(
        index === 0 ? MAIN_AGENT : `descendant-${index}`,
        index === 0 ? "Focused root" : `Descendant ${index}`,
      ),
      relation: index === 0 ? "focus" : "spawn",
      ownExecution: unavailableExecution,
      activations: { value: 0, scope: "own", complete: true },
      requiredInput: { value: null, scope: "own", complete: false },
      coverage: [],
    }),
  );
  const view = { ...traceView(), token: "directory-window" };
  const reads: URL[] = [];
  await page.route(
    (url) => url.pathname === `/api/v1/trace/agents/${MAIN_AGENT}/relations`,
    async (route) => {
      const url = new URL(route.request().url());
      reads.push(url);
      const after = url.searchParams.get("after");
      const query = url.searchParams.get("q") ?? "";
      if (after || url.searchParams.get("count_only") === "true")
        expect(url.searchParams.get("at_snapshot")).toBe(view.token);
      if (url.searchParams.get("count_only") === "true") {
        if (holdInitialCount && query === "" && !after) {
          enteredCount();
          await countGate;
          await route
            .fulfill({
              json: {
                ...tracePage([], view),
                total: { value: 999999, scope: "stale query", complete: true },
              },
            })
            .catch(() => {});
          return;
        }
        const complete = after === "count-next";
        return route.fulfill({
          json: {
            ...tracePage([], view),
            hasAfter: !complete,
            afterCursor: complete ? null : "count-next",
            state: complete ? "complete" : "work_budget_exhausted",
            total: {
              value: complete
                ? query === "needle"
                  ? 1
                  : catalogue.length
                : null,
              scope: "authorized focused agent and spawn descendants",
              complete,
            },
          },
        });
      }
      if (query === "needle") {
        if (!after)
          return route.fulfill({
            json: {
              ...tracePage([], view),
              hasAfter: true,
              afterCursor: "search-next",
              state: "work_budget_exhausted",
              total: {
                value: null,
                scope: "authorized scope",
                complete: false,
              },
            },
          });
        expect(after).toBe("search-next");
        return route.fulfill({
          json: tracePage(
            [
              {
                ...catalogue.at(-1)!,
                agent: agentLink("deep-leaf", "Needle grandchild"),
              },
            ],
            view,
          ),
        });
      }
      const offset = after === "page-next" ? traceLimits.maxItems : 0;
      const items = catalogue.slice(offset, offset + traceLimits.maxItems);
      return route.fulfill({
        json: {
          ...tracePage(items, view, offset, catalogue.length),
          beforeCursor: null,
          hasBefore: false,
          afterCursor: "page-next",
          hasAfter: offset === 0,
          total: {
            value: offset === 0 ? null : catalogue.length,
            scope: "authorized scope",
            complete: offset !== 0,
          },
        },
      });
    },
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  const directory = page.locator("[data-agent-directory]");
  await expect(
    directory.getByRole("link", { name: "Focused root", exact: true }),
  ).toBeVisible();
  return {
    state,
    reads,
    catalogue,
    view,
    directory,
    releaseCount,
    countStarted,
  };
}

test("directory search keeps its root scope and automatically advances an empty bounded prefix", async ({
  page,
}) => {
  const { state, reads, directory } = await setup(page);
  await expect(
    directory.getByText(/Focused agent and spawn descendants of Focused root/),
  ).toBeVisible();
  await expect(directory.getByText(/codex · Focused agent/)).toBeVisible();
  await page.getByLabel("Find agents").fill("needle");
  await expect(
    directory.getByRole("link", { name: "Needle grandchild", exact: true }),
  ).toBeVisible();
  expect(
    reads.some(
      (url) =>
        url.searchParams.get("q") === "needle" &&
        url.searchParams.get("after") === "search-next",
    ),
  ).toBe(true);
  expect(state.reads.some((read) => read.includes("/agents/deep-leaf"))).toBe(
    false,
  );
});

test("an exact directory count finishes through metadata passes without advancing visible rows", async ({
  page,
}) => {
  const { reads, catalogue, directory } = await setup(page);
  await expect(
    directory.getByText(new RegExp(`${catalogue.length} agents`)),
  ).toBeVisible();
  const counters = reads.filter(
    (url) => url.searchParams.get("count_only") === "true",
  );
  expect(counters.map((url) => url.searchParams.get("after"))).toEqual([
    null,
    "count-next",
  ]);
  expect(
    reads.filter((url) => url.searchParams.get("count_only") !== "true"),
  ).toHaveLength(1);
  await expect(directory.locator("[data-directory-agent]")).toHaveCount(
    traceLimits.maxItems,
  );
  await expect(
    directory.getByRole("link", { name: "Focused root", exact: true }),
  ).toBeVisible();
});

test("directory paging adopts a returned window lease and First page preserves that snapshot", async ({
  page,
}) => {
  const { reads, view, directory } = await setup(page);
  await directory
    .getByRole("button", { name: "Next page", exact: true })
    .click();
  await expect(
    directory.getByRole("link", {
      name: `Descendant ${traceLimits.maxItems}`,
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    directory.getByRole("link", { name: "Focused root", exact: true }),
  ).toHaveCount(0);
  await directory
    .getByRole("button", { name: "First page", exact: true })
    .click();
  await expect(
    directory.getByRole("link", { name: "Focused root", exact: true }),
  ).toBeVisible();
  const pages = reads.filter(
    (url) => url.searchParams.get("count_only") !== "true",
  );
  expect(pages.map((url) => url.searchParams.get("at_snapshot"))).toEqual([
    traceView().token,
    view.token,
    view.token,
  ]);
  expect(pages.map((url) => url.searchParams.get("after"))).toEqual([
    null,
    "page-next",
    null,
  ]);
});

test("changing directory search cancels an in-flight count instead of publishing a stale total", async ({
  page,
}) => {
  const failed: string[] = [];
  page.on("requestfailed", (request) => failed.push(request.url()));
  const { directory, countStarted, releaseCount } = await setup(page, true);
  await countStarted;
  await expect(
    directory.getByText("Counting authorized agents…", { exact: true }),
  ).toBeVisible();
  await page.getByLabel("Find agents").fill("needle");
  await expect(
    directory.getByRole("link", { name: "Needle grandchild", exact: true }),
  ).toBeVisible();
  releaseCount();
  await expect
    .poll(() =>
      failed.some((raw) => {
        const url = new URL(raw);
        return (
          url.searchParams.get("count_only") === "true" &&
          url.searchParams.get("q") === ""
        );
      }),
    )
    .toBe(true);
  await expect(directory).not.toContainText("999999");
  await expect(directory).not.toContainText("999,999");
});
