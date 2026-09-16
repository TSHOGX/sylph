"use client";
import { useT } from "@/lib/i18n";
import Link from "next/link";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import { traceAPI } from "@/lib/trace-api";
import {
  authorizeTraceCache,
  forgetTraceNavigation,
  onTraceAccessCleared,
  readerLocation,
  traceCacheKey,
  traceNavigationView,
} from "@/lib/trace-cache";
import { setTraceFocus } from "@/lib/trace-focus";
import { subscribeTrace } from "@/lib/trace-subscriptions";
import {
  describeCount,
  extendTraceWindow,
  type TraceWindow,
} from "@/lib/trace-window";
import { useTraceResource } from "@/lib/use-trace-resource";
import { useTracePage } from "@/lib/use-trace-page";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import {
  agentName,
  agentUrl,
  executionLabel,
  type ReadingUnit,
  type TraceView,
} from "@/lib/trace-types";
import { TracePayload } from "./trace-payload";
import {
  TraceInspector,
  type Inspection,
  type InspectionRequest,
  type ObjectInspectionTarget,
} from "./trace-inspector";
import { TraceComposer } from "./trace-composer";

export const traceButton =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";

type AgentReaderProps = {
  agent: string;
  history?: string | null;
  anchor?: string | null;
  initialView?: TraceView;
  anchorOffset?: number;
} & (
  | { companion: true; onInspect: (target: ObjectInspectionTarget) => void }
  | { companion?: false; onInspect?: never }
);

export function AgentReader({
  agent,
  history = null,
  anchor = null,
  companion = false,
  initialView,
  anchorOffset = 0,
  onInspect,
}: AgentReaderProps) {
  const t = useT();
  const ui = useWorkspaceUI();
  const [location] = useState(() => {
    const location = readerLocation(agent, history);
    if (anchor) {
      location.anchor = anchor;
      location.offset = anchorOffset;
      location.following = false;
    }
    return location;
  });
  const [navigationView] = useState(() =>
    traceNavigationView(agent, history, anchor),
  );
  const [refresh, setRefresh] = useState(0);
  const [update, setUpdate] = useState<string | null>(null);
  const [window, setWindow] = useState<TraceWindow | null>(null);
  const [paging, setPaging] = useState(false);
  const [pageError, setPageError] = useState<string | null>(null);
  const [paged, setPaged] = useState(location.paged);
  const [pagedPosition, setPagedPosition] = useState({ token: "", offset: 0 });
  const [focusedAddress, setFocusedAddress] = useState<string | null>(null);
  const [inspection, setInspection] = useState<Inspection>({ kind: "files" });
  const scroller = useRef<HTMLDivElement>(null);
  const request = useRef<AbortController | null>(null);
  const restoring = useRef(0);
  const [editorRecovery, setEditorRecovery] = useState<{
    start: number;
    end: number;
  } | null>(null);
  const bootstrap = useTraceResource(
    `reader:${agent}:${history ?? ""}`,
    (signal) =>
      traceAPI.bootstrap(
        agent,
        history,
        location.anchor,
        signal,
        refresh === 0 ? (initialView ?? navigationView) : null,
      ),
    refresh,
    false,
  );
  const data = bootstrap.data;
  const page =
    data && window?.view.token === data.view.token ? window : data?.window;
  const rows = useMemo(() => page?.items ?? [], [page]);
  const pagedStart =
    pagedPosition.token === page?.view.token ? pagedPosition.offset : 0;
  const setPagedStart = (offset: number) =>
    setPagedPosition({ token: page?.view.token ?? "", offset });
  // Two visible readers share the served row budget; the inspector never mounts a
  // directory and a child transcript together. Paged mode uses the same allocation.
  const rowBudget = Math.max(
    1,
    Math.floor((page?.limits.maxMountedRows ?? 200) / 2),
  );
  const shown = useMemo(
    () => (paged ? rows.slice(pagedStart, pagedStart + rowBudget) : rows),
    [paged, pagedStart, rows, rowBudget],
  );
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: shown.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => 120,
    overscan: 4,
    getItemKey: (index) => shown[index].address,
    rangeExtractor: (range) => {
      const indices = defaultRangeExtractor(range);
      const focused = shown.findIndex((row) => row.address === focusedAddress);
      if (focused >= 0 && !indices.includes(focused)) indices.push(focused);
      while (indices.length > rowBudget) {
        const removable = indices.findIndex(
          (index) =>
            index !== focused &&
            (index < range.startIndex || index > range.endIndex),
        );
        indices.splice(
          removable >= 0
            ? removable
            : indices.findIndex((index) => index !== focused),
          1,
        );
      }
      return indices.sort((a, b) => a - b);
    },
  });
  const remember = useCallback(() => {
    if (restoring.current) return;
    const pane = scroller.current;
    if (!pane) return;
    const bounds = pane.getBoundingClientRect();
    const visible = Array.from(
      pane.querySelectorAll<HTMLElement>("[data-trace-address]"),
    ).find((element) => element.getBoundingClientRect().bottom > bounds.top);
    if (visible) {
      location.anchor = visible.dataset.traceAnchor ?? null;
      location.offset = visible.getBoundingClientRect().top - bounds.top;
    }
    location.following =
      pane.scrollHeight - pane.scrollTop - pane.clientHeight < 48;
  }, [location]);
  const restore = useCallback(() => {
    const pane = scroller.current;
    if (!pane || !location.anchor) return;
    const index = shown.findIndex((row) => row.anchor === location.anchor);
    if (index < 0) return;
    // Scroll events from our compensation are not new reading intent. Keep the
    // object and offset immutable until measurements settle, or the estimated
    // intermediate row replaces the very anchor we are trying to restore.
    const anchor = location.anchor,
      offset = location.offset;
    const transaction = performance.now();
    restoring.current = transaction;
    virtual.shouldAdjustScrollPositionOnItemSizeChange = (item) =>
      item.index < index;
    if (!paged) virtual.scrollToIndex(index, { align: "start" });
    let frames = 0,
      settled = 0;
    const compensate = () => {
      if (restoring.current !== transaction) return;
      const element = Array.from(
        pane.querySelectorAll<HTMLElement>("[data-trace-address]"),
      ).find((node) => node.dataset.traceAnchor === anchor);
      const delta = element
        ? element.getBoundingClientRect().top -
          pane.getBoundingClientRect().top -
          offset
        : null;
      if (delta !== null && Math.abs(delta) > 0.5) {
        pane.scrollTop += delta;
        settled = 0;
      } else if (delta !== null) settled++;
      if (++frames < 30 && settled < 2) requestAnimationFrame(compensate);
      else {
        restoring.current = 0;
        location.anchor = anchor;
        location.offset = offset;
      }
    };
    requestAnimationFrame(compensate);
  }, [location, paged, shown, virtual]);
  useEffect(() => {
    if (!data) return;
    authorizeTraceCache(data.authorizationScope, data.window.limits);
    forgetTraceNavigation(agent, history, anchor);
    if (!companion) return setTraceFocus(agent, data.header.fileContext);
  }, [agent, history, anchor, companion, data]);
  useEffect(() => {
    if (!data) return;
    let queued: ReturnType<typeof setTimeout> | null = null;
    const unsubscribe = subscribeTrace(
      agent,
      data.view,
      data.changeCursor,
      data.window.limits,
      (change) => {
        if (change.kind === "access_changed") return;
        const message =
          change.kind === "reset_required"
            ? change.reason
            : t("trace.historyUpdated");
        const pane = scroller.current;
        const selection = globalThis.getSelection?.();
        const interacting =
          Boolean(selection && !selection.isCollapsed) ||
          Boolean(pane?.contains(document.activeElement));
        if (location.following && !interacting) {
          if (queued === null)
            queued = setTimeout(() => {
              queued = null;
              remember();
              setRefresh((value) => value + 1);
            }, 100);
        } else setUpdate(message);
      },
    );
    return () => {
      unsubscribe();
      if (queued !== null) clearTimeout(queued);
    };
  }, [agent, data, location, remember, t]);
  useLayoutEffect(() => {
    if (!page) return;
    if (location.following && !globalThis.getSelection?.()?.toString())
      virtual.scrollToIndex(Math.max(0, rows.length - 1), { align: "end" });
    else restore();
    const index = rows.findIndex((row) => row.anchor === location.anchor);
    virtual.shouldAdjustScrollPositionOnItemSizeChange =
      index >= 0 ? (item) => item.index < index : undefined;
  }, [page, location, rows, restore, virtual]);
  useEffect(
    () =>
      onTraceAccessCleared((reason) => {
        if (reason !== "view") return;
        const focused = document.activeElement;
        if (
          focused instanceof HTMLTextAreaElement &&
          focused.hasAttribute("data-composer-editor")
        )
          setEditorRecovery({
            start: focused.selectionStart,
            end: focused.selectionEnd,
          });
        remember();
        setUpdate(t("trace.leaseExpired"));
        setRefresh((value) => value + 1);
      }),
    [remember, t],
  );
  useEffect(
    () => () => {
      request.current?.abort();
      restoring.current = 0;
      remember();
    },
    [remember],
  );
  const loadPage = async (direction: "before" | "after") => {
    if (!data || !page || paging) return;
    if (
      paged &&
      ((direction === "before" && pagedStart > 0) ||
        (direction === "after" && pagedStart + rowBudget < rows.length))
    ) {
      const nextStart =
        direction === "before"
          ? Math.max(0, pagedStart - rowBudget)
          : pagedStart + rowBudget;
      location.anchor = rows[nextStart]?.anchor ?? null;
      location.offset = 0;
      location.following = false;
      setPagedStart(nextStart);
      return;
    }
    const cursor =
      direction === "before" ? page.beforeCursor : page.afterCursor;
    if (!cursor) return;
    remember();
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setPaging(true);
    setPageError(null);
    try {
      const next = await traceAPI.history(
        agent,
        page.view,
        history,
        direction,
        cursor,
        controller.signal,
      );
      if (!controller.signal.aborted) {
        if (paged) {
          location.anchor = next.items[0]?.anchor ?? null;
          location.offset = 0;
          location.following = false;
          setPagedStart(0);
        }
        setWindow(paged ? next : extendTraceWindow(page, next, direction));
      }
    } catch (error) {
      if (!controller.signal.aborted)
        setPageError(error instanceof Error ? error.message : String(error));
    } finally {
      if (!controller.signal.aborted) setPaging(false);
    }
  };
  const inspect = (next: InspectionRequest) => {
    if (!data) return;
    remember();
    const target: Inspection =
      next.kind === "object"
        ? {
            ...next,
            scope: {
              agent: data.header.agent.address,
              history: data.header.history.selection,
              view: data.view,
              changeCursor: data.changeCursor,
              maximumPayloadBytes: data.window.limits.maxPayloadBytes,
              anchor: location.anchor,
              offset: location.offset,
            },
          }
        : next;
    if (companion && target.kind === "object") {
      onInspect?.(target);
      return;
    }
    setInspection(target);
    if (ui.isMobile) ui.openDrawer("files");
    else ui.setRightOpen(true);
  };
  const acceptUpdate = () => {
    remember();
    setUpdate(null);
    setRefresh((value) => value + 1);
  };
  const view = data?.view;
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <section
        className="flex min-h-0 min-w-0 flex-1 flex-col"
        aria-label={
          data
            ? `${agentName(data.header.agent, t)} conversation`
            : t("trace.conversation")
        }
      >
        <header className="shrink-0 border-b border-line px-3 py-2">
          <h1 className="truncate text-base font-semibold">
            {data ? agentName(data.header.agent, t) : "Loading conversation…"}
          </h1>
          {data && (
            <>
              <p className="mt-1 truncate text-xs text-ink-muted">
                {data.header.environmentName ?? data.header.environment} ·{" "}
                {data.header.history.label} ·{" "}
                {executionLabel(data.header.ownExecution, t)}
              </p>
              <div className="flex flex-wrap items-center gap-x-2 text-xs">
                {data.header.parent && (
                  <Link
                    className={traceButton}
                    href={agentUrl(data.header.parent.address)}
                  >
                    {t("trace.parent", {
                      agent: agentName(data.header.parent, t),
                    })}
                  </Link>
                )}
                {data.header.origin && (
                  <Link
                    className={traceButton}
                    href={agentUrl(data.header.origin.address)}
                  >
                    {t("trace.origin", {
                      agent: agentName(data.header.origin, t),
                    })}
                  </Link>
                )}
              </div>
              {!companion && (
                <nav aria-label="Conversation tools" className="flex flex-wrap">
                  <button
                    className={traceButton}
                    onClick={() => inspect({ kind: "find" })}
                  >
                    {t("trace.findAgent")}
                  </button>
                  <button
                    className={traceButton}
                    onClick={() => inspect({ kind: "agents" })}
                  >
                    {t("trace.agents")}
                  </button>
                  <button
                    className={traceButton}
                    onClick={() => inspect({ kind: "history" })}
                  >
                    {t("trace.history")}
                  </button>
                  <button
                    className={traceButton}
                    onClick={() => inspect({ kind: "coverage" })}
                  >
                    {t("trace.details")}
                  </button>
                </nav>
              )}
            </>
          )}
        </header>
        {(update || bootstrap.error) && (
          <div
            className="shrink-0 border-b border-line px-3 py-2"
            role="status"
          >
            <p>{update ?? bootstrap.error?.message}</p>
            <button className={traceButton} onClick={acceptUpdate}>
              {t("trace.refreshAround")}
            </button>
            {bootstrap.error && (
              <button
                className={traceButton}
                onClick={() => {
                  location.anchor = null;
                  location.offset = 0;
                  location.following = true;
                  setRefresh((value) => value + 1);
                }}
              >
                {t("trace.latestHistory")}
              </button>
            )}
          </div>
        )}
        <div
          ref={scroller}
          data-trace-scroller
          onFocusCapture={(event) =>
            setFocusedAddress(
              (event.target as Element).closest<HTMLElement>(
                "[data-trace-address]",
              )?.dataset.traceAddress ?? null,
            )
          }
          onBlurCapture={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget))
              setFocusedAddress(null);
          }}
          className="min-h-0 min-w-0 flex-1 overflow-auto px-3 [overflow-anchor:none]"
          onScroll={remember}
          onWheel={() => {
            restoring.current = 0;
            location.following = false;
          }}
          onPointerDown={() => {
            restoring.current = 0;
            location.following = false;
          }}
          onKeyDown={() => {
            restoring.current = 0;
            location.following = false;
          }}
        >
          {page && view && (
            <>
              <div className="flex flex-wrap items-center justify-between py-2">
                <button
                  className={traceButton}
                  disabled={
                    (!page.hasBefore && (!paged || pagedStart === 0)) || paging
                  }
                  onClick={() => void loadPage("before")}
                >
                  {t("trace.earlier")}
                </button>
                <button
                  className={traceButton}
                  aria-pressed={paged}
                  onClick={() => {
                    remember();
                    location.paged = !paged;
                    setPagedStart(
                      Math.floor(
                        Math.max(
                          0,
                          rows.findIndex(
                            (row) => row.anchor === location.anchor,
                          ),
                        ) / rowBudget,
                      ) * rowBudget,
                    );
                    setPaged(!paged);
                  }}
                >
                  {t(paged ? "trace.pagedOn" : "trace.pagedOff")}
                </button>
              </div>
              {bootstrap.pending && (
                <p role="status" className="text-sm text-ink-muted">
                  {t("trace.updating")}
                </p>
              )}
              {!rows.length && !bootstrap.pending && (
                <p className="mx-auto max-w-prose py-12 text-sm text-ink-muted">
                  {t("trace.noContent")}
                </p>
              )}
              {paged ? (
                <div>
                  {shown.map((row) => (
                    <TraceRow
                      key={row.address}
                      row={row}
                      view={view}
                      maximum={page.limits.maxPayloadBytes}
                      inspect={inspect}
                    />
                  ))}
                </div>
              ) : (
                <div
                  style={{
                    height: virtual.getTotalSize(),
                    position: "relative",
                  }}
                >
                  {virtual
                    .getVirtualItems()
                    .slice(0, rowBudget)
                    .map((item) => (
                      <div
                        key={item.key}
                        data-index={item.index}
                        ref={virtual.measureElement}
                        style={{
                          position: "absolute",
                          top: 0,
                          left: 0,
                          width: "100%",
                          transform: `translateY(${item.start}px)`,
                        }}
                      >
                        <TraceRow
                          row={shown[item.index]}
                          view={view}
                          maximum={page.limits.maxPayloadBytes}
                          inspect={inspect}
                        />
                      </div>
                    ))}
                </div>
              )}
              {pageError && (
                <p role="alert" className="py-2">
                  {pageError}
                </p>
              )}
              <button
                className={traceButton}
                disabled={
                  (!page.hasAfter &&
                    (!paged || pagedStart + rowBudget >= rows.length)) ||
                  paging
                }
                onClick={() => void loadPage("after")}
              >
                {t("trace.later")}
              </button>
              <p className="pb-3 text-xs text-ink-muted">
                {describeCount(page.total, t("trace.readingNoun"), t)} ·{" "}
                {page.total.scope}
                {page.state === "work_budget_exhausted"
                  ? ` · ${t("trace.moreRequired")}`
                  : ""}
              </p>
            </>
          )}
          {!data && !bootstrap.error && (
            <p role="status" className="py-12 text-center">
              {t("trace.loadingHistory")}
            </p>
          )}
        </div>
        {data &&
          (companion ? (
            <Link
              className={`${traceButton} border-t border-line text-center`}
              href={agentUrl(agent, history, location.anchor)}
            >
              {t("trace.openConversation")}
            </Link>
          ) : (
            <TraceComposer
              bootstrap={data}
              onFind={() => inspect({ kind: "find" })}
              restoreSelection={editorRecovery}
              onSelectionRestored={() => setEditorRecovery(null)}
            />
          ))}
      </section>
      {!companion && data && (
        <TraceInspector
          key={agent}
          bootstrap={data}
          inspection={inspection}
          onInspect={setInspection}
        />
      )}
    </div>
  );
}

function TraceRow({
  row,
  view,
  maximum,
  inspect,
}: {
  row: ReadingUnit;
  view: TraceView;
  maximum: number;
  inspect: (next: InspectionRequest) => void;
}) {
  const t = useT();
  const content = row.content;
  let body;
  switch (content.kind) {
    case "message":
      body = (
        <>
          <h2 className="mb-2 text-sm font-medium">
            {content.author.kind === "unknown"
              ? "Input · source unknown"
              : content.author.label}
            {content.purpose === "task_input" && content.author.kind === "agent"
              ? ` · ${t("trace.taskInput")}`
              : ""}
          </h2>
          {content.originalOwner && (
            <p className="mb-2 text-xs">
              {t("trace.inherited")}{" "}
              <Link
                className="underline"
                href={agentUrl(content.originalOwner.address)}
              >
                {agentName(content.originalOwner, t)}
              </Link>
            </p>
          )}
          <div className="space-y-3">
            {content.parts.map((part) => (
              <div key={part.partRef}>
                {part.kind === "disclosed_reasoning" && (
                  <p className="text-xs text-ink-muted">
                    {t("trace.reasoning")}
                  </p>
                )}
                <TracePayload
                  access={part.payload}
                  view={view}
                  maximum={maximum}
                  preview={part.preview}
                  truncated={part.previewTruncated}
                />
              </div>
            ))}
          </div>
          {!content.partsComplete && (
            <MessageParts
              via={row.anchor}
              selection={content.parts[0]?.payload.selection ?? null}
              revision={content.revisionRef}
              cursor={content.partsCursor}
              view={view}
              maximum={maximum}
              total={content.partsTotal}
            />
          )}
          {content.delivery && (
            <button
              className={traceButton}
              onClick={() =>
                inspect({
                  kind: "object",
                  reference: content.delivery!.reference,
                })
              }
            >
              {t("trace.delivery")}:{" "}
              {content.delivery.stages
                .map((stage) => stage.kind.replaceAll("_", " "))
                .join(" · ") || t("trace.evidenceUnavailable")}
            </button>
          )}
        </>
      );
      break;
    case "work":
      body = (
        <button
          className={`${traceButton} w-full border border-line text-left`}
          onClick={() =>
            inspect({ kind: "object", reference: content.operationsAnchor })
          }
        >
          {t("trace.work")} ·{" "}
          {describeCount(content.operations, t("trace.operations"), t)} ·{" "}
          {describeCount(content.agents, t("trace.agentsNoun"), t)}
          <span className="mt-1 block text-xs">
            {describeCount(
              content.activeResources,
              t("trace.resourcesNoun"),
              t,
            )}{" "}
            · {describeCount(content.requiredInput, t("trace.requestsNoun"), t)}
          </span>
        </button>
      );
      break;
    case "activation_boundary":
      body = (
        <>
          <h2 className="text-sm font-medium">
            {executionLabel(content.execution, t)}
          </h2>
          {content.execution.durations.map((duration, index) => (
            <p key={index} className="text-xs text-ink-muted">
              {duration.value} {duration.unit} ·{" "}
              {duration.basis.replaceAll("_", " ")}
            </p>
          ))}
          <p className="text-xs text-ink-muted">
            {describeCount(content.triggerCount, t("trace.triggersNoun"), t)}
          </p>
        </>
      );
      break;
    case "context_boundary":
      body = (
        <button
          className={traceButton}
          onClick={() =>
            inspect({ kind: "object", reference: content.checkpoint })
          }
        >
          {content.label} · {t("trace.inspectContext")}
        </button>
      );
      break;
    case "history_boundary":
      body = <h2 className="text-sm font-medium">{content.label}</h2>;
      break;
    case "unknown":
      body = (
        <>
          <h2 className="text-sm font-medium">{content.label}</h2>
          <p className="text-sm">{content.reason}</p>
          {content.payloads.map((payload, index) => (
            <TracePayload
              key={index}
              access={payload}
              view={view}
              maximum={maximum}
            />
          ))}
        </>
      );
      break;
  }
  return (
    <article
      data-trace-address={row.address}
      data-trace-anchor={row.anchor}
      className={`mx-auto my-3 max-w-[78ch] break-words rounded px-3 py-3 ${content.kind === "message" && content.author.kind === "human" ? "border border-line bg-sunken" : ""}`}
    >
      {body}
      <div className="mt-2 flex flex-wrap items-center justify-between gap-2 text-xs text-ink-muted">
        <span>
          {row.sourceSurface}
          {row.orderBasis === "presentation_tiebreak"
            ? ` · ${t("trace.orderUncertain")}`
            : ""}
        </span>
        <button
          className="min-h-11 rounded px-2 hover:bg-hover focus-ring"
          onClick={() => inspect({ kind: "object", reference: row.anchor })}
        >
          {t("trace.evidenceDetails")}
        </button>
      </div>
    </article>
  );
}

function MessageParts({
  via,
  selection,
  revision,
  cursor,
  view,
  maximum,
  total,
}: {
  via: string;
  selection: string | null;
  revision: string;
  cursor: string | null;
  view: TraceView;
  maximum: number;
  total: number;
}) {
  const t = useT();
  const result = useTracePage(
    traceCacheKey(view, ["parts", revision, via, selection]),
    cursor,
    (next, signal) =>
      traceAPI.parts(revision, view, next, via, selection, signal),
  );
  return (
    <div>
      {result.data?.items.map((part) => (
        <TracePayload
          key={part.partRef}
          access={part.payload}
          view={view}
          maximum={maximum}
          preview={part.preview}
          truncated={part.previewTruncated}
        />
      ))}
      {result.error && <p role="alert">{result.error.message}</p>}
      <button
        className={traceButton}
        disabled={result.disabled}
        onClick={result.load}
      >
        {result.error
          ? t("trace.queryRetry")
          : t("trace.moreParts", { count: total })}
      </button>
    </div>
  );
}
