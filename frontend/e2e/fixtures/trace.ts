import { expect, type Page } from "@playwright/test";
import type { PendingRequest } from "../../lib/api-types";
import type {
  AgentDirectoryRow,
  AgentHeader,
  CommandReceipt,
  CommandRequest,
  ControlAvailability,
  ObjectDetail,
  PayloadAccess,
  ReaderBootstrap,
  ReadingUnit,
  ResultRepresentation,
  ResultOccurrence,
  ResultRepresentationEntry,
  TraceLimits,
  TracePage,
  TraceView,
} from "../../lib/trace-types";

export const MAIN_AGENT = "opaque-main-agent";
export const CHILD_AGENT = "opaque-embedded-child";
export const traceLimits: TraceLimits = {
  maxItems: 40,
  maxSummaryBytes: 262144,
  maxPreviewBytes: 1024,
  maxPayloadBytes: 4096,
  maxChangeItems: 100,
  maxMountedRows: 200,
  maxMetadataCacheBytes: 16 * 1024 * 1024,
  maxPayloadCacheBytes: 32 * 1024 * 1024,
  maxHiddenLocations: 10,
  maxVisibleInterests: 2,
};
export const traceView = (revision = 1): TraceView => ({
  token: `view-${revision}`,
  generation: "generation-one",
  evidenceCut: revision,
  viewRevision: revision,
  expiresAtMs: Date.now() + 3600000,
});
const count = (value: number, scope = "Selected agent history") => ({
  value,
  scope,
  complete: true,
});
export const agentLink = (
  address = MAIN_AGENT,
  name: string | null = "Main agent",
) => ({ address, name, vendor: "codex", identitySuffix: address.slice(-8) });
export function resultCatalogue(
  available: ResultRepresentation[] = [],
): ResultRepresentationEntry[] {
  const representations: ResultRepresentation[] = [
    "runtime",
    "caller_result",
    "conversation",
  ];
  return representations.map((representation) => ({
    representation,
    state: {
      kind: available.includes(representation) ? "available" : "unobserved",
    },
  }));
}
export const unavailableExecution = {
  reference: null,
  state: { kind: "unavailable" as const },
  durations: [],
};
export function payload(
  reference: string,
  viaRef = "history-anchor",
  agent = MAIN_AGENT,
): PayloadAccess {
  return {
    payloadRef: reference,
    bodyRevision: "revision-1",
    representation: "conversation",
    viaRef,
    containingAgent: agent,
    selection: null,
    mediaType: "text/plain",
    encoding: "utf8",
    totalBytes: null,
    availableRanges: [],
    availability: { kind: "available" },
  };
}
export function message(
  index: number,
  text = `Retained message ${index}`,
  agent = MAIN_AGENT,
): ReadingUnit {
  return {
    address: `${agent}-entry-${index}`,
    anchor: `${agent}-anchor-${index}`,
    orderBasis: "native_predecessor",
    sourceSurface: "Retained conversation",
    activation: null,
    content: {
      kind: "message",
      messageRef: `message-${index}`,
      revisionRef: `message-revision-${index}`,
      author: {
        kind: "agent",
        agent: agentLink(agent),
        label: agent === MAIN_AGENT ? "Main agent" : "Child agent",
      },
      purpose: "unclassified",
      delivery: null,
      parts: [
        {
          partRef: `part-${index}`,
          kind: "text",
          preview: text,
          previewTruncated: false,
          payload: payload(`body-${index}`, `${agent}-anchor-${index}`, agent),
        },
      ],
      partsTotal: 1,
      partsComplete: true,
      partsCursor: null,
      originalOwner: null,
    },
  };
}
export function tracePage<T>(
  items: T[],
  view = traceView(),
  start = 0,
  total = items.length,
): TracePage<T> {
  return {
    view,
    items,
    beforeCursor: start > 0 ? String(start) : null,
    afterCursor:
      start + items.length < total ? String(start + items.length) : null,
    hasBefore: start > 0,
    hasAfter: start + items.length < total,
    total: count(total),
    state: start + items.length < total ? "more" : "complete",
    returnedItems: items.length,
    returnedBytes: JSON.stringify(items).length,
    limits: traceLimits,
  };
}
export function header(agent = MAIN_AGENT): AgentHeader {
  return {
    agent: agentLink(agent, agent === MAIN_AGENT ? "Main agent" : null),
    environment: "fixture-environment",
    environmentName: "Fixture machine",
    model: null,
    ownExecution: unavailableExecution,
    history: {
      selection: null,
      label: "Current history",
      isCurrent: true,
      selectionKnown: true,
      availability: { kind: "available" },
    },
    parent: agent === MAIN_AGENT ? null : agentLink(),
    origin: null,
    coverage: [
      {
        dimension: "Execution boundary",
        state: {
          kind: "unavailable",
          reason: "Source did not report an execution boundary",
        },
        evidence: null,
      },
    ],
    descendantCount: count(0),
    continuingResources: count(0),
    requiredInput: count(0),
    fileContext: null,
  };
}
export function bootstrap(
  agent = MAIN_AGENT,
  rows = [message(0)],
  view = traceView(),
): ReaderBootstrap {
  return {
    view,
    header: header(agent),
    window: tracePage(rows, view),
    changeCursor: String(view.viewRevision),
    progress: {
      evidenceCut: view.evidenceCut,
      projectionCut: view.evidenceCut,
      searchCut: view.evidenceCut,
      pendingOutbox: 0,
      oldestOutboxAgeMs: null,
      sourceConnected: true,
      fault: null,
    },
    control: { kind: "unavailable", reason: "No independent control route" },
    authorizationScope: "fixture-subject",
  };
}

/** Typed server projection fixtures. No browser fixture parses a vendor payload. */
export async function installTraceFixture(page: Page, rows = [message(0)]) {
  const state = {
    revision: 1,
    rows,
    childRows: [message(1, "The child owns this answer", CHILD_AGENT)],
    mainHeader: header(),
    childHeader: header(CHILD_AGENT),
    catalogue: [] as AgentDirectoryRow[],
    control: {
      kind: "unavailable",
      reason: "No independent control route",
    } as ControlAvailability,
    details: new Map<string, ObjectDetail>(),
    results: new Map<string, ResultOccurrence[]>(),
    bodies: new Map<string, string>(),
    commands: [] as CommandRequest[],
    requests: [] as PendingRequest[],
    receipts: [] as CommandReceipt[],
    reads: [] as string[],
    searched: [] as string[],
    failPayload: false,
  };
  await page.addInitScript(() =>
    localStorage.setItem(
      "sylph-web.settings.v1",
      JSON.stringify({ showRightSidebar: false, showLeftSidebar: false }),
    ),
  );
  await page.route("**/api/v1/sessions/fixture-session/capabilities", (route) =>
    route.fulfill({
      json: {
        link: { observe: { available: true }, control: { available: true } },
        supports: ["send", "configure"],
        capabilities: { config: [], commands: [], subagents: [] },
      },
    }),
  );
  await page.route("**/api/v1/sessions/fixture-session/requests", (route) =>
    route.fulfill({ json: { requests: state.requests } }),
  );
  await page.route("**/api/v1/trace/**", async (route) => {
    const url = new URL(route.request().url());
    const pieces = url.pathname.split("/").map(decodeURIComponent);
    const q = url.searchParams;
    const resource = pieces[4],
      id = pieces[5],
      action = pieces[6];
    state.reads.push(
      `${route.request().method()} ${url.pathname}${url.search}`,
    );
    if (resource === "views") return route.fulfill({ status: 204 });
    const view = q.get("at_snapshot")
      ? traceView(Number(q.get("at_snapshot")!.split("-")[1]))
      : traceView(state.revision);
    if (resource === "changes") {
      const change =
        state.revision > Number(q.get("cursor") ?? 0)
          ? `data: ${JSON.stringify({ kind: "revision", cursor: String(state.revision), generation: view.generation, evidenceCut: state.revision, viewRevision: state.revision, agents: [MAIN_AGENT], changed: [state.rows[0]?.anchor], moved: [], removed: [] })}\n\n`
          : ": heartbeat\n\n";
      return route.fulfill({ contentType: "text/event-stream", body: change });
    }
    if (resource === "agents" && !action) {
      const source = id === MAIN_AGENT ? state.rows : state.childRows;
      const around = q.get("around");
      const anchorIndex = source.findIndex((row) => row.anchor === around);
      const start =
        around && anchorIndex >= 0
          ? Math.max(0, anchorIndex - 5)
          : Math.max(0, source.length - traceLimits.maxItems);
      const result = bootstrap(id, [], view);
      result.header = id === MAIN_AGENT ? state.mainHeader : state.childHeader;
      result.window = tracePage(
        source.slice(start, start + traceLimits.maxItems),
        view,
        start,
        source.length,
      );
      return route.fulfill({ json: result });
    }
    if (resource === "agents" && action === "history") {
      const source = id === MAIN_AGENT ? state.rows : state.childRows;
      const start = q.has("before")
        ? Math.max(0, Number(q.get("before")) - traceLimits.maxItems)
        : Number(q.get("after") ?? 0);
      return route.fulfill({
        json: tracePage(
          source.slice(start, start + traceLimits.maxItems),
          view,
          start,
          source.length,
        ),
      });
    }
    if (resource === "agents" && action === "control")
      return route.fulfill({
        json:
          id === MAIN_AGENT
            ? state.control
            : {
                kind: "unavailable",
                reason: "Embedded child has no independent control route",
              },
      });
    if (resource === "agents" && action === "commands")
      return route.fulfill({ json: { commands: state.receipts } });
    if (resource === "agents" && action === "relations") {
      const query = q.get("q") ?? "";
      state.searched.push(query);
      const filtered = state.catalogue.filter((row) =>
        (row.agent.name ?? "").toLowerCase().includes(query.toLowerCase()),
      );
      const start = Number(q.get("after") ?? 0);
      return route.fulfill({
        json: tracePage(
          filtered.slice(start, start + traceLimits.maxItems),
          view,
          start,
          filtered.length,
        ),
      });
    }
    if (resource === "agents" && action === "activations")
      return route.fulfill({ json: tracePage([], view) });
    if (resource === "agents" && action === "histories")
      return route.fulfill({
        json: tracePage(
          [
            {
              selection: "historical-branch",
              label: "Earlier branch",
              isCurrent: false,
              availability: { kind: "available" },
              entryCount: count(12),
            },
          ],
          view,
        ),
      });
    if (
      resource === "objects" &&
      action === "relations" &&
      q.get("kind") === "results"
    ) {
      const representation = q.get("representation");
      if (!representation)
        return route.fulfill({
          status: 400,
          json: {
            error: {
              code: "invalid_cursor",
              message: "ResultRepresentation required",
            },
          },
        });
      const results = (state.results.get(id) ?? []).filter(
        (result) => result.representation === representation,
      );
      const limit = Math.min(
        Number(q.get("limit") ?? 16),
        traceLimits.maxItems,
      );
      const start = q.has("before")
        ? Math.max(0, Number(q.get("before")) - limit)
        : Number(q.get("after") ?? 0);
      return route.fulfill({
        json: tracePage(
          results.slice(start, start + limit),
          view,
          start,
          results.length,
        ),
      });
    }
    if (resource === "objects") {
      const detail = state.details.get(id);
      if (detail) return route.fulfill({ json: { ...detail, view } });
      return route.fulfill({
        json: {
          view,
          reference: id,
          title: "Retained evidence",
          owner: agentLink(),
          execution: null,
          availability: { kind: "available" },
          content: { kind: "generic", label: "Evidence", payloads: [] },
          relations: [],
          relationsCursor: null,
          coverage: [],
        },
      });
    }
    if (resource === "payloads") {
      if (state.failPayload)
        return route.fulfill({
          status: 403,
          json: { error: { code: "FORBIDDEN", message: "Access removed" } },
        });
      const bytes = Buffer.from(
        state.bodies.get(id) ?? "Retained complete body",
      );
      const start = Number(q.get("start") ?? 0),
        end = Math.min(Number(q.get("end") ?? bytes.length), bytes.length);
      return route.fulfill({
        status: 206,
        headers: {
          "Content-Type": "text/plain",
          ...(end > start
            ? { "Content-Range": `bytes ${start}-${end - 1}/${bytes.length}` }
            : {}),
        },
        body: bytes.subarray(start, end),
      });
    }
    if (resource === "commands" && route.request().method() === "POST") {
      const command = route.request().postDataJSON() as CommandRequest;
      state.commands.push(command);
      if (command.action === "approval" && command.answer) {
        const index = state.requests.findIndex(
          (request) => request.id === command.answer!.requestId,
        );
        if (index >= 0) state.requests.splice(index, 1);
      }
      const receipt: CommandReceipt = {
        commandId: `command-${state.commands.length}`,
        observationRef: {
          issuer: { kind: "server", value: "fixture" },
          producerId: "fixture-outbox",
          producerEpoch: "run",
          producerSeq: state.commands.length,
        },
        state: "accepted",
        revision: 1,
        text: command.text,
        error: null,
        editable: command.action === "queue",
        cancellable: command.action === "queue",
      };
      state.receipts.push(receipt);
      return route.fulfill({ status: 202, json: receipt });
    }
    if (resource === "search")
      return route.fulfill({
        json: tracePage(
          [
            {
              anchor: `${MAIN_AGENT}-anchor-3`,
              agent: agentLink(),
              history: state.mainHeader.history,
              excerpt: "A retained hit outside the visible window",
              payload: null,
            },
          ],
          view,
        ),
      });
    return route.fulfill({
      status: 404,
      json: {
        error: {
          code: "NOT_FOUND",
          message: `No fixture for ${resource}/${action}`,
        },
      },
    });
  });
  return state;
}
export async function openTrace(
  page: Page,
  agent = MAIN_AGENT,
  anchor?: string,
) {
  await page.goto(`/agents/${agent}${anchor ? `?anchor=${anchor}` : ""}`);
  await expect(
    page
      .getByRole("heading", {
        name: agent === MAIN_AGENT ? "Main agent" : /Unnamed agent/,
      })
      .first(),
  ).toBeVisible();
}
