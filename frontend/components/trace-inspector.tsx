"use client";
import { useT } from "@/lib/i18n";
import dynamic from "next/dynamic";
import Link from "next/link";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  EyeIcon,
  RefreshIcon,
} from "./icons";
import {
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { traceAPI } from "@/lib/trace-api";
import { rememberTraceNavigation, traceCacheKey } from "@/lib/trace-cache";
import { retainTraceView } from "@/lib/trace-leases";
import { subscribeTrace } from "@/lib/trace-subscriptions";
import { useTraceResource } from "@/lib/use-trace-resource";
import { useTracePage } from "@/lib/use-trace-page";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useDialog } from "@/lib/use-dialog";
import {
  agentName,
  agentUrl,
  availabilityLabel,
  executionLabel,
  type ObjectDetail,
  type ReaderBootstrap,
  type TraceView,
} from "@/lib/trace-types";
import { describeCount } from "@/lib/trace-window";
import { ResourceHistory, RevisionPayloads } from "./trace-resource-history";
import { TracePayload } from "./trace-payload";
import { OperationResults } from "./trace-operation-results";
import { TraceEvidence } from "./trace-evidence";
import { OriginDirectory, type OriginLocation } from "./trace-origins";
import { ArtifactsPane } from "./artifacts-pane";
const Playback = dynamic(() =>
  import("./trace-playback").then((module) => module.TracePlayback),
);
const EvidenceExport = dynamic(() =>
  import("./trace-evidence-export").then(
    (module) => module.TraceEvidenceExport,
  ),
);
const Companion = dynamic(() =>
  import("./agent-reader").then((module) => module.AgentReader),
);
const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
export interface InspectionScope {
  agent: string;
  history: string | null;
  view: TraceView;
  changeCursor: string;
  maximumPayloadBytes: number;
  anchor: string | null;
  offset: number;
}
export type OriginInspectionTarget = {
  kind: "origins";
  agent: string;
  view: TraceView;
  changeCursor: string;
  maximum: number;
};
export type InspectionRequest =
  | OriginInspectionTarget
  | { kind: "agents" | "find" | "history" | "coverage" | "files" }
  | { kind: "object"; reference: string; agent?: string }
  | {
      kind: "agent";
      agent: string;
      history?: string | null;
      view?: TraceView;
      anchor?: string | null;
      offset?: number;
    };
export type ObjectInspectionTarget = {
  kind: "object";
  reference: string;
  scope: InspectionScope;
};
export type Inspection =
  Exclude<InspectionRequest, { kind: "object" }> | ObjectInspectionTarget;
interface DirectoryLocation {
  query: string;
  filter: string;
  cursor: string | null;
  view: TraceView | null;
  position: { agent: string; offset: number } | null;
}

export function TraceInspector({
  bootstrap,
  inspection,
  onInspect,
}: {
  bootstrap: ReaderBootstrap;
  inspection: Inspection;
  onInspect: (value: Inspection) => void;
}) {
  const t = useT();
  const ui = useWorkspaceUI();
  const [available, setAvailable] = useState(0);
  const [trail, setTrail] = useState<Inspection[]>([]);
  const [pinned, setPinned] = useState<
    Array<{ target: ObjectInspectionTarget; label: string }>
  >([]);
  const [originPages, setOriginPages] = useState<
    Record<string, OriginLocation>
  >({});
  const [width, setWidth] = useState(480);
  const [directory, setDirectory] = useState<DirectoryLocation>({
    query: "",
    filter: "all",
    cursor: null,
    view: null,
    position: null,
  });

  // A detail may replace the companion that acquired its lease. Keep the exact
  // view alive across that handoff and bounded Back/pin entries; never substitute
  // the main reader's view for a child's already-authorized selection.
  useLayoutEffect(() => {
    const views = new Map<string, TraceView>();
    for (const target of [
      inspection,
      ...trail,
      ...pinned.map((item) => item.target),
    ]) {
      const view =
        target.kind === "object"
          ? target.scope.view
          : target.kind === "agent" || target.kind === "origins"
            ? target.view
            : null;
      if (view) views.set(view.token, view);
    }
    if (directory.view) views.set(directory.view.token, directory.view);
    const releases = [...views.values()].map(retainTraceView);
    return () => releases.forEach((release) => release());
  }, [inspection, trail, pinned, directory.view]);
  const visible = ui.isMobile ? ui.drawer === "files" : ui.rightOpen;
  const [observationReset, setObservationReset] = useState<string | null>(null);
  const resetInspection = useEffectEvent((reason: string) =>
    setObservationReset(reason),
  );
  const subscribeInspection = useEffectEvent((scope: InspectionScope) =>
    subscribeTrace(
      scope.agent,
      scope.view,
      scope.changeCursor,
      bootstrap.window.limits,
      (change) => {
        if (change.kind === "reset_required") resetInspection(change.reason);
      },
    ),
  );
  const { maxVisibleInterests, maxSummaryBytes, maxChangeItems } =
    bootstrap.window.limits;
  useEffect(() => {
    if (!visible) return;
    const scopes = new Map<string, InspectionScope>();
    for (const target of [inspection, ...pinned.map((item) => item.target)]) {
      if (target.kind === "origins") {
        scopes.set(JSON.stringify([target.agent, target.view.token]), {
          agent: target.agent,
          history: null,
          view: target.view,
          changeCursor: target.changeCursor,
          maximumPayloadBytes: target.maximum,
          anchor: null,
          offset: 0,
        });
      }
      if (target.kind === "object")
        scopes.set(
          JSON.stringify([target.scope.agent, target.scope.view.token]),
          target.scope,
        );
    }
    // Detail snapshots remain protected after their companion unmounts. They join
    // the same bounded connection so a revoked child never leaves a cached preview.
    const releases = [...scopes.values()].map(subscribeInspection);
    return () => releases.forEach((release) => release());
  }, [
    visible,
    inspection,
    pinned,
    maxVisibleInterests,
    maxSummaryBytes,
    maxChangeItems,
  ]);
  const full = available < 940;
  const close = () => {
    if (ui.isMobile) ui.closeDrawer();
    else ui.setRightOpen(false);
  };
  const { ref, onKeyDown } = useDialog<HTMLDivElement>({
    enabled: visible && full && !ui.artifactFullscreen,
    onClose: close,
  });
  const tabs = useRef<HTMLDivElement>(null);
  const selected =
    inspection.kind === "files"
      ? "files"
      : inspection.kind === "agents" || inspection.kind === "agent"
        ? "agents"
        : "details";
  useEffect(() => {
    const parent = document.getElementById("main");
    if (!parent) return;
    const observer = new ResizeObserver(([entry]) =>
      setAvailable(entry.contentRect.width),
    );
    observer.observe(parent);
    return () => observer.disconnect();
  }, []);
  const navigate = (next: Inspection) => {
    const previous =
      inspection.kind === "agent" && next.kind === "object"
        ? {
            ...inspection,
            history: next.scope.history,
            view: next.scope.view,
            anchor: next.scope.anchor,
            offset: next.scope.offset,
          }
        : inspection;
    setTrail((entries) => [...entries, previous].slice(-10));
    onInspect(next);
  };
  const inspect = (next: InspectionRequest) => {
    let scope =
      inspection.kind === "object"
        ? inspection.scope
        : {
            agent: bootstrap.header.agent.address,
            history: bootstrap.header.history.selection,
            view: bootstrap.view,
            changeCursor: bootstrap.changeCursor,
            maximumPayloadBytes: bootstrap.window.limits.maxPayloadBytes,
            anchor: null,
            offset: 0,
          };
    if (inspection.kind === "origins") {
      scope = {
        ...scope,
        agent: inspection.agent,
        history: null,
        view: inspection.view,
        changeCursor: inspection.changeCursor,
        maximumPayloadBytes: inspection.maximum,
      };
    }
    if (next.kind === "object" && next.agent && next.agent !== scope.agent) {
      scope = {
        ...scope,
        agent: next.agent,
        history: null,
        anchor: next.reference,
        offset: 0,
      };
    }
    navigate(next.kind === "object" ? { ...next, scope } : next);
  };
  const back = () => {
    const previous = trail.at(-1);
    if (!previous) return;
    setTrail((items) => items.slice(0, -1));
    onInspect(previous);
  };
  const resize = (next: number) =>
    setWidth(Math.max(420, Math.min(640, available - 520, next)));
  if (!visible) return null;
  const style = full
    ? undefined
    : { width: Math.max(420, Math.min(width, available - 520)) };
  return (
    <div
      ref={ref}
      aria-label={t("trace.inspectorLabel")}
      role={full && !ui.artifactFullscreen ? "dialog" : "complementary"}
      aria-modal={(full && !ui.artifactFullscreen) || undefined}
      onKeyDown={onKeyDown}
      style={style}
      className={`${full ? "fixed inset-0 z-40" : "relative shrink-0 border-l border-line"} flex min-h-0 min-w-0 flex-col bg-canvas`}
    >
      {!full && (
        <div
          role="separator"
          aria-label="Resize inspector"
          aria-orientation="vertical"
          aria-valuemin={420}
          aria-valuemax={Math.min(640, available - 520)}
          aria-valuenow={Math.round(style!.width)}
          tabIndex={0}
          className="absolute bottom-0 left-0 top-0 z-10 w-2 cursor-col-resize focus-ring"
          onKeyDown={(event) => {
            if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
              event.preventDefault();
              resize(width + (event.key === "ArrowLeft" ? 20 : -20));
            }
          }}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
          }}
          onPointerMove={(event) => {
            if (event.currentTarget.hasPointerCapture(event.pointerId))
              resize(
                event.currentTarget.parentElement!.getBoundingClientRect()
                  .right - event.clientX,
              );
          }}
        />
      )}
      <header className="flex shrink-0 items-center justify-between border-b border-line px-3 py-1">
        <h2 className="text-sm font-semibold">
          {full ? t("trace.inspectConversation") : t("trace.inspector")}
        </h2>
        <button className={button} onClick={close}>
          {full ? t("trace.backConversation") : t("trace.close")}
        </button>
      </header>
      {observationReset && (
        <p
          role="status"
          className="shrink-0 border-b border-line px-3 py-2 text-sm"
        >
          {observationReset}
        </p>
      )}
      <div
        ref={tabs}
        role="tablist"
        aria-label="Inspector destinations"
        className="flex shrink-0 border-b border-line px-2"
        onKeyDown={(event) => {
          const buttons = Array.from(
            tabs.current?.querySelectorAll<HTMLButtonElement>("[role=tab]") ??
              [],
          );
          const index = buttons.indexOf(
            document.activeElement as HTMLButtonElement,
          );
          const next =
            event.key === "ArrowRight"
              ? (index + 1) % buttons.length
              : event.key === "ArrowLeft"
                ? (index + buttons.length - 1) % buttons.length
                : event.key === "Home"
                  ? 0
                  : event.key === "End"
                    ? buttons.length - 1
                    : null;
          if (next !== null) {
            event.preventDefault();
            buttons[next]?.focus();
          }
        }}
      >
        {(["agents", "details", "files"] as const).map((tab) => (
          <button
            key={tab}
            role="tab"
            id={`inspector-${tab}`}
            aria-controls={`inspector-panel-${tab}`}
            aria-selected={selected === tab}
            tabIndex={selected === tab ? 0 : -1}
            className={button}
            onClick={() =>
              inspect({ kind: tab === "details" ? "coverage" : tab })
            }
          >
            {t(
              (
                {
                  agents: "trace.agents",
                  details: "trace.details",
                  files: "trace.files",
                } as const
              )[tab],
            )}
          </button>
        ))}
      </div>
      {(trail.length > 0 ||
        pinned.length > 0 ||
        inspection.kind === "object") && (
        <div className="flex shrink-0 flex-wrap items-center gap-1 border-b border-line px-2">
          {trail.length > 0 && (
            <button className={button} onClick={back}>
              {t("trace.previousDetail")}
            </button>
          )}
          {inspection.kind === "object" && (
            <button
              className={button}
              disabled={
                pinned.length >= 4 ||
                pinned.some(
                  (item) =>
                    item.target.reference === inspection.reference &&
                    item.target.scope.view.token ===
                      inspection.scope.view.token,
                )
              }
              onClick={() =>
                setPinned((previous) => [
                  ...previous,
                  {
                    target: inspection,
                    label: t("trace.pinnedLabel", {
                      count: previous.length + 1,
                    }),
                  },
                ])
              }
            >
              {t("trace.pinDetail")}
            </button>
          )}
          {pinned.map((item) => (
            <span
              key={`${item.target.reference}:${item.target.scope.view.token}`}
              className="flex items-center"
            >
              <button className={button} onClick={() => navigate(item.target)}>
                {item.label}
              </button>
              <button
                className={button}
                aria-label={t("trace.unpin", { label: item.label })}
                onClick={() =>
                  setPinned((previous) =>
                    previous.filter((value) => value !== item),
                  )
                }
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      <div
        role="tabpanel"
        id={`inspector-panel-${selected}`}
        aria-labelledby={`inspector-${selected}`}
        className="flex min-h-0 min-w-0 flex-1 flex-col"
      >
        {inspection.kind === "agent" ? (
          <>
            <button
              className={`${button} shrink-0 text-left`}
              onClick={() => {
                const directoryIndex = trail.findLastIndex(
                  (entry) => entry.kind === "agents",
                );
                setTrail((entries) =>
                  directoryIndex >= 0 ? entries.slice(0, directoryIndex) : [],
                );
                onInspect({ kind: "agents" });
              }}
            >
              {t("trace.allAgents")}
            </button>
            <Companion
              key={inspection.agent}
              agent={inspection.agent}
              history={inspection.history}
              anchor={inspection.anchor}
              anchorOffset={inspection.offset}
              initialView={inspection.view}
              onInspect={navigate}
              companion
            />
          </>
        ) : inspection.kind === "agents" ? (
          <AgentDirectory
            bootstrap={bootstrap}
            location={directory}
            onLocation={setDirectory}
            alongside={(agent) => inspect({ kind: "agent", agent })}
          />
        ) : inspection.kind === "files" ? (
          bootstrap.header.fileContext ? (
            <ArtifactsPane />
          ) : (
            <p className="p-4 text-sm">{t("trace.noFiles")}</p>
          )
        ) : (
          <div className="min-h-0 flex-1 overflow-auto p-3">
            {inspection.kind === "find" && (
              <AgentSearch bootstrap={bootstrap} />
            )}
            {inspection.kind === "origins" && (
              <OriginDirectory
                key={traceCacheKey(inspection.view, [
                  inspection.agent,
                  "origins",
                ])}
                {...inspection}
                request={
                  originPages[
                    traceCacheKey(inspection.view, [inspection.agent])
                  ] ?? { cursor: null, direction: "after", attempt: 0 }
                }
                setRequest={(value) =>
                  setOriginPages((old) =>
                    Object.fromEntries([
                      ...Object.entries(old)
                        .filter(
                          ([key]) =>
                            key !==
                            traceCacheKey(inspection.view, [inspection.agent]),
                        )
                        .slice(-9),
                      [
                        traceCacheKey(inspection.view, [inspection.agent]),
                        value,
                      ],
                    ]),
                  )
                }
                onInspect={inspect}
              />
            )}
            {inspection.kind === "history" && (
              <>
                <HistoryDirectory bootstrap={bootstrap} />
                <ActivationDirectory
                  bootstrap={bootstrap}
                  onInspect={(reference, view) =>
                    navigate({
                      kind: "object",
                      reference,
                      scope: {
                        agent: bootstrap.header.agent.address,
                        history: null,
                        view,
                        changeCursor: bootstrap.changeCursor,
                        maximumPayloadBytes:
                          bootstrap.window.limits.maxPayloadBytes,
                        anchor: null,
                        offset: 0,
                      },
                    })
                  }
                />
              </>
            )}
            {inspection.kind === "coverage" && (
              <Coverage bootstrap={bootstrap} />
            )}
            {inspection.kind === "object" && (
              <ObjectInspection
                key={`${inspection.reference}:${inspection.scope.view.token}:${inspection.scope.history ?? ""}`}
                reference={inspection.reference}
                history={inspection.scope.history}
                view={inspection.scope.view}
                maximum={inspection.scope.maximumPayloadBytes}
                onInspect={inspect}
              />
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function AgentDirectory({
  bootstrap,
  location,
  onLocation,
  alongside,
}: {
  bootstrap: ReaderBootstrap;
  location: DirectoryLocation;
  onLocation: (value: DirectoryLocation) => void;
  alongside: (agent: string) => void;
}) {
  const t = useT();
  const scroller = useRef<HTMLDivElement>(null);
  const restored = useRef(false);
  const { query, filter, cursor } = location;
  const view = location.view ?? bootstrap.view;
  const address = bootstrap.header.agent.address;
  const result = useTraceResource(
    traceCacheKey(view, ["agents", address, query, filter, cursor]),
    (signal) => traceAPI.agents(address, view, query, filter, cursor, signal),
    undefined,
    false,
  );
  const advanceEmpty = useEffectEvent(() => {
    const page = result.data;
    if (
      !result.pending &&
      !result.error &&
      page?.state === "work_budget_exhausted" &&
      page.items.length === 0 &&
      page.hasAfter &&
      page.afterCursor
    ) {
      onLocation({
        ...location,
        view: page.view,
        cursor: page.afterCursor,
        position: null,
      });
    }
  });
  useEffect(() => {
    advanceEmpty();
  }, [result.data, result.pending, result.error]);
  useLayoutEffect(() => {
    if (
      restored.current ||
      result.pending ||
      !result.data ||
      !location.position ||
      !scroller.current
    )
      return;
    const pane = scroller.current;
    const row = [
      ...pane.querySelectorAll<HTMLElement>("[data-directory-agent]"),
    ].find((item) => item.dataset.directoryAgent === location.position!.agent);
    if (!row) return;
    const offset = location.position.offset;
    row
      .querySelector<HTMLButtonElement>("[data-open-alongside]")
      ?.focus({ preventScroll: true });
    pane.scrollTop +=
      row.getBoundingClientRect().top -
      pane.getBoundingClientRect().top -
      offset;
    restored.current = true;
  }, [result.data, result.pending, location.position]);
  const update = (patch: Partial<DirectoryLocation>) =>
    onLocation({
      ...location,
      view: result.data?.view ?? view,
      position: null,
      ...patch,
    });
  const open = (agent: string, row: HTMLElement) => {
    const pane = scroller.current;
    onLocation({
      ...location,
      view: result.data?.view ?? view,
      position: pane
        ? {
            agent,
            offset:
              row.getBoundingClientRect().top -
              pane.getBoundingClientRect().top,
          }
        : null,
    });
    alongside(agent);
  };
  return (
    <div
      ref={scroller}
      data-agent-directory
      className="min-h-0 flex-1 space-y-3 overflow-auto p-3 [overflow-anchor:none]"
    >
      <label className="block text-sm">
        {t("trace.findAgents")}
        <input
          className="mt-1 min-h-11 w-full rounded border border-line bg-canvas px-3 text-base"
          value={query}
          onChange={(event) => {
            update({ query: event.target.value, cursor: null });
          }}
        />
      </label>
      <label className="block text-sm">
        {t("trace.show")}
        <select
          className="ml-2 min-h-11 rounded border border-line bg-canvas px-2"
          value={filter}
          onChange={(event) => {
            update({ filter: event.target.value, cursor: null });
          }}
        >
          {[
            ["all", t("trace.all")],
            ["active", t("trace.observedActive")],
            ["needs_input", t("trace.needsMe")],
            ["completed", t("trace.completed")],
            ["unknown", t("trace.outcomeUnknown")],
          ].map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <p className="text-xs text-ink-muted">
        {t("trace.scopeAgents", {
          agent: agentName(bootstrap.header.agent, t),
        })}{" "}
        {result.data &&
          (result.data.total.complete ? (
            describeCount(result.data.total, t("trace.agentsNoun"), t)
          ) : (
            <DirectoryCount
              key={traceCacheKey(result.data.view, [
                address,
                query,
                filter,
                "count",
              ])}
              agent={address}
              view={result.data.view}
              query={query}
              filter={filter}
            />
          ))}
      </p>
      {result.error && <p role="alert">{result.error.message}</p>}
      {result.pending && <p role="status">{t("trace.loadingAgents")}</p>}
      <ul className="space-y-2">
        {result.data?.items.map((row) => (
          <li
            key={row.agent.address}
            data-directory-agent={row.agent.address}
            className="rounded border border-line p-3"
          >
            <Link
              className="block break-words font-medium underline"
              href={agentUrl(row.agent.address)}
            >
              {agentName(row.agent, t)}
            </Link>
            <p className="mt-1 text-xs text-ink-muted">
              {row.agent.vendor} ·{" "}
              {row.relation === "focus"
                ? t("trace.focusedAgent")
                : t("trace.spawnDescendant")}{" "}
              · {executionLabel(row.ownExecution, t)}
            </p>
            <p className="mt-1 text-xs">
              {describeCount(row.activations, t("trace.activationsNoun"), t)} ·{" "}
              {describeCount(row.requiredInput, t("trace.requestsNoun"), t)}
            </p>
            {row.coverage
              .filter((coverage) => coverage.state.kind !== "available")
              .map((coverage, index) => (
                <p key={index} className="text-xs">
                  {coverage.dimension}: {availabilityLabel(coverage.state)}
                </p>
              ))}
            <button
              className={button}
              data-open-alongside
              onClick={(event) =>
                open(
                  row.agent.address,
                  event.currentTarget.closest<HTMLElement>(
                    "[data-directory-agent]",
                  )!,
                )
              }
            >
              {t("trace.alongside")}
            </button>
          </li>
        ))}
      </ul>
      <div className="mt-3 flex flex-wrap">
        <button
          className={button}
          disabled={cursor === null || result.pending}
          onClick={() => update({ cursor: null })}
        >
          {t("trace.firstPage")}
        </button>
        <button
          className={button}
          disabled={!result.data?.hasAfter || result.pending}
          onClick={() => update({ cursor: result.data?.afterCursor ?? null })}
        >
          {t("trace.nextPage")}
        </button>
      </div>
    </div>
  );
}

function DirectoryCount({
  agent,
  view,
  query,
  filter,
}: {
  agent: string;
  view: TraceView;
  query: string;
  filter: string;
}) {
  const t = useT();
  const result = useTraceResource(
    traceCacheKey(view, ["directory-count", agent, query, filter]),
    async (signal) => {
      let cursor: string | null = null;
      for (;;) {
        const page = await traceAPI.directoryCount(
          agent,
          view,
          query,
          filter,
          cursor,
          signal,
        );
        if (!page.hasAfter || !page.afterCursor) return page;
        cursor = page.afterCursor;
      }
    },
    undefined,
    false,
  );
  return (
    <span role="status">
      {result.data?.total.complete
        ? describeCount(result.data.total, t("trace.agentsNoun"), t)
        : t(
            result.error
              ? "trace.directoryCountUnavailable"
              : "trace.directoryCounting",
          )}
    </span>
  );
}

function HistoryDirectory({ bootstrap }: { bootstrap: ReaderBootstrap }) {
  const t = useT();
  const [cursor, setCursor] = useState<string | null>(null);
  const result = useTraceResource(
    traceCacheKey(bootstrap.view, [
      "histories",
      bootstrap.header.agent.address,
      cursor,
    ]),
    (signal) =>
      traceAPI.histories(
        bootstrap.header.agent.address,
        bootstrap.view,
        cursor,
        signal,
      ),
  );
  return (
    <div>
      <h3 className="mb-3 font-medium">{t("trace.histories")}</h3>
      {result.error && <p role="alert">{result.error.message}</p>}
      <ul>
        {result.data?.items.map((history) => (
          <li key={history.selection} className="border-b border-line py-3">
            <Link
              className="underline"
              href={agentUrl(bootstrap.header.agent.address, history.selection)}
            >
              {history.label}
              {history.isCurrent ? " · Current" : ""}
            </Link>
            <p className="text-xs text-ink-muted">
              {describeCount(history.entryCount, t("trace.entriesNoun"), t)}{" "}
              {availabilityLabel(history.availability)}
            </p>
          </li>
        ))}
      </ul>
      <PageControls
        page={result.data}
        pending={result.pending}
        setCursor={setCursor}
      />
    </div>
  );
}

function ActivationDirectory({
  bootstrap,
  onInspect,
}: {
  bootstrap: ReaderBootstrap;
  onInspect: (reference: string, view: TraceView) => void;
}) {
  const t = useT();
  const [position, setPosition] = useState<{
    direction: "before" | "after";
    cursor: string;
  } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const result = useTraceResource(
    traceCacheKey(bootstrap.view, [
      "activations",
      bootstrap.header.agent.address,
      position,
    ]),
    (signal) =>
      traceAPI.activations(
        bootstrap.header.agent.address,
        bootstrap.view,
        position,
        signal,
      ),
    attempt,
  );
  const page = result.data;
  return (
    <section
      aria-label={t("trace.ownActivations")}
      className="mt-4 border-t border-line pt-4"
    >
      <h3 className="font-medium">{t("trace.ownActivations")}</h3>
      {page && (
        <p className="mt-1 text-xs text-ink-muted">
          {describeCount(page.total, t("trace.activationsNoun"), t)}
        </p>
      )}
      {result.pending && <p role="status">{t("trace.loadingDetails")}</p>}
      {result.error && <p role="alert">{result.error.message}</p>}
      {result.error && (
        <button
          className={button}
          aria-label={t("trace.retry")}
          title={t("trace.retry")}
          onClick={() => setAttempt((value) => value + 1)}
        >
          <RefreshIcon />
        </button>
      )}
      <ul>
        {page?.items.map((activation) => (
          <li
            key={activation.reference}
            className="flex min-h-11 items-center justify-between gap-2 border-b border-line py-2"
          >
            <span className="min-w-0 text-sm">
              {executionLabel(activation, t)}
            </span>
            <button
              className={button}
              disabled={!activation.reference}
              aria-label={t("trace.inspectActivation")}
              title={t("trace.inspectActivation")}
              onClick={() =>
                activation.reference &&
                onInspect(activation.reference, page.view)
              }
            >
              <EyeIcon />
            </button>
          </li>
        ))}
      </ul>
      <nav aria-label={t("trace.ownActivations")} className="mt-2 flex">
        <button
          className={button}
          aria-label={t("trace.previousPage")}
          title={t("trace.previousPage")}
          disabled={result.pending || !page?.hasBefore || !page.beforeCursor}
          onClick={() =>
            page?.beforeCursor &&
            setPosition({ direction: "before", cursor: page.beforeCursor })
          }
        >
          <ChevronLeftIcon />
        </button>
        <button
          className={button}
          aria-label={t("trace.nextPage")}
          title={t("trace.nextPage")}
          disabled={result.pending || !page?.hasAfter || !page.afterCursor}
          onClick={() =>
            page?.afterCursor &&
            setPosition({ direction: "after", cursor: page.afterCursor })
          }
        >
          <ChevronRightIcon />
        </button>
      </nav>
    </section>
  );
}

function AgentSearch({ bootstrap }: { bootstrap: ReaderBootstrap }) {
  const t = useT();
  const [input, setInput] = useState("");
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState<string | null>(null);
  const [searchView, setSearchView] = useState<TraceView | null>(null);
  const result = useTraceResource(
    query
      ? traceCacheKey(bootstrap.view, [
          "search",
          bootstrap.header.agent.address,
          query,
          cursor,
        ])
      : null,
    (signal) =>
      traceAPI
        .search(
          bootstrap.header.agent.address,
          bootstrap.header.history.selection,
          query,
          cursor,
          signal,
          searchView,
        )
        .then((page) => {
          setSearchView(page.view);
          return page;
        }),
  );
  return (
    <div>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          setQuery(input.trim());
          setCursor(null);
          setSearchView(null);
        }}
      >
        <label className="block text-sm">
          {t("trace.findIn", { agent: agentName(bootstrap.header.agent, t) })}
          <input
            value={input}
            onChange={(event) => setInput(event.target.value)}
            className="mt-2 min-h-11 w-full rounded border border-line bg-canvas px-3 text-base"
          />
        </label>
        <button className={button} type="submit">
          {t("trace.searchHistory")}
        </button>
      </form>
      <p className="my-2 text-xs text-ink-muted">
        {t("trace.indexProgress", {
          index: bootstrap.progress.searchCut ?? t("trace.unknown"),
          evidence: bootstrap.progress.evidenceCut,
        })}
      </p>
      {result.error && <p role="alert">{result.error.message}</p>}
      {result.pending && <p role="status">{t("trace.searching")}</p>}
      <ul className="space-y-3">
        {result.data?.items.map((hit) => (
          <li key={hit.anchor} className="rounded border border-line p-3">
            <p className="whitespace-pre-wrap text-sm">{hit.excerpt}</p>
            <Link
              className={`${button} mt-2 inline-block underline`}
              href={agentUrl(
                hit.agent.address,
                hit.history.selection,
                hit.anchor,
              )}
              onClick={() => {
                if (result.data)
                  rememberTraceNavigation(
                    hit.agent.address,
                    hit.history.selection,
                    hit.anchor,
                    result.data.view,
                  );
              }}
            >
              {t("trace.resultLink", { history: hit.history.label })}
            </Link>
          </li>
        ))}
      </ul>
      <PageControls
        page={result.data}
        pending={result.pending}
        setCursor={setCursor}
      />
    </div>
  );
}

function PageControls({
  page,
  pending,
  setCursor,
}: {
  page?: {
    hasBefore: boolean;
    hasAfter: boolean;
    beforeCursor: string | null;
    afterCursor: string | null;
  };
  pending: boolean;
  setCursor: (cursor: string | null) => void;
}) {
  const t = useT();
  return (
    <div className="mt-3 flex flex-wrap">
      <button
        className={button}
        disabled={!page?.hasBefore || pending}
        onClick={() => setCursor(page?.beforeCursor ?? null)}
      >
        {t("trace.previousPage")}
      </button>
      <button
        className={button}
        disabled={!page?.hasAfter || pending}
        onClick={() => setCursor(page?.afterCursor ?? null)}
      >
        {t("trace.nextPage")}
      </button>
    </div>
  );
}

function Coverage({ bootstrap }: { bootstrap: ReaderBootstrap }) {
  const t = useT();
  return (
    <div className="space-y-4">
      <h3 className="font-medium">{t("trace.coverage")}</h3>
      <p className="text-sm">
        {executionLabel(bootstrap.header.ownExecution, t)}
      </p>
      <ul className="space-y-2">
        {bootstrap.header.coverage.map((coverage, index) => (
          <li key={index}>
            <span className="text-sm font-medium">{coverage.dimension}</span>
            <p className="text-sm">
              {coverage.state.kind === "available"
                ? t("trace.available")
                : coverage.state.reason}
            </p>
          </li>
        ))}
      </ul>
      <Playback
        key={bootstrap.header.environment}
        environment={bootstrap.header.environment}
        environmentName={bootstrap.header.environmentName}
        view={bootstrap.view}
      />
      <EvidenceExport
        environment={bootstrap.header.environment}
        environmentName={bootstrap.header.environmentName}
        view={bootstrap.view}
      />
      <dl className="grid grid-cols-2 gap-2 text-sm">
        <dt>{t("trace.connection")}</dt>
        <dd>
          {bootstrap.progress.sourceConnected
            ? t("trace.connected")
            : t("trace.disconnected")}
        </dd>
        <dt>{t("trace.admitted")}</dt>
        <dd>{bootstrap.progress.evidenceCut}</dd>
        <dt>{t("trace.projectionReady")}</dt>
        <dd>{bootstrap.progress.projectionCut}</dd>
        <dt>{t("trace.searchIndexed")}</dt>
        <dd>{bootstrap.progress.searchCut ?? "Unavailable"}</dd>
        <dt>Accepted, history updating</dt>
        <dd>{bootstrap.progress.pendingOutbox}</dd>
      </dl>
      {bootstrap.progress.fault && (
        <p role="alert">{bootstrap.progress.fault}</p>
      )}
      <p className="text-xs text-ink-muted">
        {t("trace.viewMeta", {
          revision: bootstrap.view.viewRevision,
          generation: bootstrap.view.generation,
        })}
      </p>
    </div>
  );
}

function ObjectInspection({
  reference,
  history,
  view,
  maximum,
  onInspect,
}: {
  reference: string;
  history: string | null;
  view: TraceView;
  maximum: number;
  onInspect: (value: InspectionRequest) => void;
}) {
  const t = useT();
  const result = useTraceResource(
    traceCacheKey(view, ["object", reference, history]),
    (signal) => traceAPI.object(reference, view, history, signal),
  );
  return (
    <div className="space-y-4">
      {result.error ? (
        <p role="alert">{result.error.message}</p>
      ) : !result.data ? (
        <p role="status">{t("trace.loadingDetails")}</p>
      ) : (
        <ObjectContent
          detail={result.data}
          history={history}
          maximum={maximum}
          onInspect={onInspect}
        />
      )}
      <TraceEvidence
        key={traceCacheKey(view, [reference, history, "evidence"])}
        reference={reference}
        history={history}
        view={view}
        maximum={maximum}
        onInspect={onInspect}
      />
    </div>
  );
}

function ObjectContent({
  detail,
  history,
  maximum,
  onInspect,
}: {
  detail: ObjectDetail;
  history: string | null;
  maximum: number;
  onInspect: (value: InspectionRequest) => void;
}) {
  const t = useT();
  const content = detail.content;
  return (
    <div className="space-y-4">
      <header>
        <h3 className="break-words text-base font-semibold">{detail.title}</h3>
        {detail.owner && (
          <Link
            className="text-sm underline"
            href={agentUrl(detail.owner.address)}
          >
            {t("trace.owner", { agent: agentName(detail.owner, t) })}
          </Link>
        )}
        {detail.execution && (
          <p className="text-sm">{executionLabel(detail.execution, t)}</p>
        )}
        {detail.availability.kind !== "available" && (
          <p>{detail.availability.reason}</p>
        )}
      </header>
      {content.kind === "operation" ? (
        <>
          <p className="text-xs text-ink-muted">{content.category.kind}</p>
          {content.input.length > 0 && (
            <h4 className="font-medium">{t("trace.input")}</h4>
          )}
          {content.input.map((payload, index) => (
            <TracePayload
              key={index}
              access={payload}
              view={detail.view}
              maximum={maximum}
              label={t("trace.input")}
            />
          ))}
          <OperationResults
            key={traceCacheKey(detail.view, [
              detail.reference,
              history,
              "results",
            ])}
            reference={detail.reference}
            history={history}
            view={detail.view}
            catalogue={content.resultRepresentations}
            maximum={maximum}
          />
          {content.resources.map((relation) => (
            <button
              key={relation.target}
              className={button}
              onClick={() =>
                onInspect({
                  kind: "object",
                  reference: relation.target,
                  agent: relation.containingAgent,
                })
              }
            >
              {relation.label}
            </button>
          ))}
          {content.resourcesCursor && (
            <RelationPages
              reference={detail.reference}
              history={history}
              cursor={content.resourcesCursor}
              view={detail.view}
              kind="resources"
              onInspect={onInspect}
            />
          )}
        </>
      ) : content.kind === "resource" ? (
        <ResourceHistory
          reference={detail.reference}
          history={history}
          view={detail.view}
          observations={content.observations}
          cursor={content.observationsCursor}
          maximum={maximum}
          onInspect={(reference) => onInspect({ kind: "object", reference })}
        />
      ) : content.kind === "revision" ? (
        <RevisionPayloads
          reference={detail.reference}
          history={history}
          view={detail.view}
          representation={content.representation}
          form={content.form}
          onInspect={(reference) => onInspect({ kind: "object", reference })}
          payloads={content.payloads}
          cursor={content.payloadsCursor}
          maximum={maximum}
        />
      ) : content.kind === "measurement" ? (
        <dl className="space-y-2 text-sm">
          <dt>{t("trace.metric")}</dt>
          <dd>
            {content.metric.kind === "extension"
              ? content.metric.name
              : content.metric.kind.replaceAll("_", " ")}
          </dd>
          <dt>{t("trace.value")}</dt>
          <dd>
            {content.value}{" "}
            {content.unit.kind === "currency"
              ? content.unit.code
              : content.unit.kind === "extension"
                ? content.unit.name
                : content.unit.kind}
          </dd>
          <dt>{t("trace.scope")}</dt>
          <dd>{content.scope}</dd>
          {content.model && (
            <>
              <dt>{t("composer.model")}</dt>
              <dd>
                {content.model.reportedName}
                {content.model.provider ? ` · ${content.model.provider}` : ""}
              </dd>
            </>
          )}
          <dt>{t("trace.interpretation")}</dt>
          <dd>{content.mode.kind}</dd>
        </dl>
      ) : (
        <>
          {content.kind === "context" && (
            <>
              <p className="text-sm">
                {content.basis} · {content.coverage.kind}
              </p>
              {content.memberships.map((member) => (
                <button
                  key={member.target}
                  className={button}
                  onClick={() =>
                    onInspect({ kind: "object", reference: member.target })
                  }
                >
                  {member.label}
                </button>
              ))}
              {content.membershipsCursor && (
                <RelationPages
                  reference={detail.reference}
                  history={history}
                  cursor={content.membershipsCursor}
                  view={detail.view}
                  kind="memberships"
                  onInspect={onInspect}
                />
              )}
            </>
          )}
          {content.payloads.map((payload, index) => (
            <TracePayload
              key={index}
              access={payload}
              view={detail.view}
              maximum={maximum}
            />
          ))}
        </>
      )}
      {detail.relationsCursor && (
        <RelationPages
          reference={detail.reference}
          history={history}
          cursor={detail.relationsCursor}
          view={detail.view}
          kind="relations"
          onInspect={onInspect}
        />
      )}
      {detail.relations.length > 0 && (
        <section>
          <h4 className="font-medium">{t("trace.related")}</h4>
          <ul>
            {detail.relations.map((relation) => (
              <li key={relation.target}>
                <button
                  className={button}
                  onClick={() =>
                    onInspect({
                      kind: "object",
                      reference: relation.target,
                      agent: relation.containingAgent,
                    })
                  }
                >
                  {relation.label}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

/** Drafts have no trace identity, but their explicitly prepared file context is usable. */
export function StandaloneFiles() {
  const t = useT();
  const ui = useWorkspaceUI();
  const visible = ui.isMobile ? ui.drawer === "files" : ui.rightOpen;
  if (!visible) return null;
  return (
    <aside
      aria-label={t("trace.files")}
      className={`${ui.isMobile ? "fixed inset-0 z-40" : "w-96 border-l border-line"} flex min-h-0 flex-col bg-canvas`}
    >
      <button
        className={button}
        onClick={() =>
          ui.isMobile ? ui.closeDrawer() : ui.setRightOpen(false)
        }
      >
        {t("trace.backConversation")}
      </button>
      <ArtifactsPane />
    </aside>
  );
}

function RelationPages({
  reference,
  history,
  cursor,
  view,
  kind,
  onInspect,
}: {
  reference: string;
  history: string | null;
  cursor: string;
  view: TraceView;
  kind: "resources" | "memberships" | "relations";
  onInspect: (value: InspectionRequest) => void;
}) {
  const t = useT();
  const result = useTracePage(
    traceCacheKey(view, [reference, history, kind]),
    cursor,
    (next, signal) =>
      traceAPI.relations(reference, view, history, kind, next, signal),
  );
  return (
    <div>
      {result.error && <p role="alert">{result.error.message}</p>}
      <ul>
        {result.data?.items.map((relation) => (
          <li key={relation.target}>
            <button
              className={button}
              onClick={() =>
                onInspect({
                  kind: "object",
                  reference: relation.target,
                  agent: relation.containingAgent,
                })
              }
            >
              {relation.label}
            </button>
          </li>
        ))}
      </ul>
      <button
        className={button}
        disabled={result.disabled}
        onClick={result.load}
      >
        {result.error
          ? t("trace.queryRetry")
          : `${t("trace.moreSearch")} · ${kind}`}
      </button>
    </div>
  );
}
