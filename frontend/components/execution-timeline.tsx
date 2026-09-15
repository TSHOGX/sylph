"use client";

import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type RefObject,
} from "react";
import { useQuery } from "@tanstack/react-query";
import { useVirtualizer, type Virtualizer } from "@tanstack/react-virtual";
import { readingView } from "@/lib/reading-view-state";
import { useT } from "@/lib/i18n";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useSessionIdParam } from "@/lib/use-session-id-param";
import {
  activatePanel,
  updatePanel,
  panelSnapshot,
} from "@/lib/work-panel-state";
import { formatDuration } from "@/lib/format";
import type { Fidelity } from "@/lib/api-types";
import { api, segment } from "@/lib/api";
import {
  ExecutionStore,
  reportedUsage,
  type ExecutionNode,
} from "@/lib/execution-store";
import { Markdown } from "./markdown";
import { EventBody } from "./event-body";
import {
  ChevronRightIcon,
  BranchIcon,
  TerminalIcon,
  FileTextIcon,
  PencilIcon,
  SearchIcon,
  SparklesIcon,
  ToolIcon,
} from "./icons";
import { interiorMessage } from "@/lib/interior-state";
import { describeTool } from "@/lib/tool-shape";
import { DiffView } from "./diff-view";
import { OutputBlock } from "./code-block";

function label(node: ExecutionNode) {
  if (node.trajectory?.name) return node.trajectory.name;
  if (node.event.kind !== "execution") {
    if (node.event.kind === "request")
      return node.event.text || "Input request";
    if (node.event.kind === "thinking") return "Thinking";
    if (node.event.kind === "tool_call") {
      const shape = describeTool(node.event);
      return [shape.name, shape.command || shape.path]
        .filter(Boolean)
        .join(" · ");
    }
    return node.event.name ?? node.event.kind;
  }
  const other = node.tools - node.reads - node.commands;
  return [
    node.thinking ? "Thinking" : "",
    node.reads ? `${node.reads} file reads` : "",
    node.commands ? `ran ${node.commands} commands` : "",
    other ? `${other} tool calls` : "",
    !node.thinking && !node.tools ? "Execution" : "",
  ]
    .filter(Boolean)
    .join(", ");
}

export function ExecutionTimeline({
  sessionId,
  scroller,
  collapseEpoch,
  onOpenFile,
  find,
  turns,
  onResend,
  isLive,
  onReviewRequest,
  fidelity,
  root = "",
  view = "main",
}: {
  view?: string;
  root?: string;
  sessionId: string;
  scroller: RefObject<HTMLDivElement | null>;
  collapseEpoch: number;
  onOpenFile: (path: string, line?: number) => void;
  find?: string | null;
  onResend: (text: string) => void;
  isLive: boolean;
  onReviewRequest: (id: string) => void;
  fidelity: Fidelity;
  turns: { turn: { id: string; status: string } }[];
}) {
  const store = useMemo(
    () => readingView(sessionId, root, view),
    [sessionId, root, view],
  );
  const virtualRef = useRef<Virtualizer<HTMLDivElement, Element> | null>(null);
  const subscribe = useMemo(
    () => (fn: () => void) =>
      store.subscribe("", () => {
        const pane = scroller.current;
        const virtual = virtualRef.current;
        if (pane && virtual) {
          const viewport = pane.getBoundingClientRect();
          const anchor = Array.from(
            pane.querySelectorAll<HTMLElement>("[data-node-id]"),
          ).find((element) => {
            const rect = element.getBoundingClientRect();
            return rect.bottom > viewport.top && rect.top < viewport.bottom;
          });
          const index = store
            .snapshot()
            .findIndex((row) => row.id === anchor?.dataset.nodeId);
          // With controls above the first row, the reading anchor can sit below
          // the viewport edge. A newly prepended row in that gap must compensate
          // its estimated height too. Virtual's default viewport-edge predicate
          // misses it; retain the actual node until the next reading gesture.
          virtual.shouldAdjustScrollPositionOnItemSizeChange =
            anchor && index >= 0 ? (item) => item.index < index : undefined;
        }
        fn();
      }),
    [store, scroller],
  );
  const rows = useSyncExternalStore(subscribe, store.snapshot, store.snapshot);
  const top = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const [margin, setMargin] = useState(0);
  const measuredMargin = useRef(0);
  const [query, setQuery] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setQuery(find?.trim() ?? ""), 250);
    return () => clearTimeout(timer);
  }, [find]);
  const search = useQuery({
    queryKey: ["session", sessionId, "node-search", query],
    enabled: query.length >= 2,
    queryFn: async ({ signal }) => {
      const response = await fetch(
        `/api/v1/sessions/${segment(sessionId)}/nodes/search?q=${encodeURIComponent(query)}`,
        { signal, credentials: "include" },
      );
      if (!response.ok) throw new Error(`Search failed (${response.status})`);
      return response.json() as Promise<{
        hits: { seq: number; excerpt: string }[];
      }>;
    },
  });
  useEffect(() => {
    const seq = new URLSearchParams(window.location.search).get("seq");
    if (seq !== null && /^\d+$/.test(seq)) void store.seek(Number(seq));
  }, [store]);
  useLayoutEffect(() => {
    const measure = () => {
      const pane = scroller.current;
      if (!top.current || !pane) return;
      const next =
        top.current.getBoundingClientRect().top -
        pane.getBoundingClientRect().top +
        pane.scrollTop;
      const previous = measuredMargin.current;
      if (next !== previous) {
        measuredMargin.current = next;
        if (pane.scrollTop > previous) pane.scrollTop += next - previous;
        setMargin(next);
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    if (content.current) observer.observe(content.current);
    return () => observer.disconnect();
  }, [scroller, store]);
  useEffect(() => {
    store.start();
    const pane = scroller.current;
    if (pane && store.scrollTop) pane.scrollTop = store.scrollTop;
    return () => {
      if (pane) store.scrollTop = pane.scrollTop;
      store.stop();
    };
  }, [store, scroller]);
  useEffect(() => {
    store.active = isLive;
  }, [store, isLive]);
  const previousCollapse = useRef(collapseEpoch);
  useEffect(() => {
    if (previousCollapse.current !== collapseEpoch) {
      previousCollapse.current = collapseEpoch;
      store.collapseAll();
      for (const node of store.nodes.values())
        if (node.event.kind === "work")
          store.disclosureChoices.set(node.id, false);
    }
  }, [store, collapseEpoch]);
  // Variable-height measurements preserve the visible anchor when preceding rows
  // resize. Only the expanded, loaded list enters the virtualizer.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => 44,
    overscan: 6,
    getItemKey: (index) => rows[index].id,
    scrollMargin: margin,
    anchorTo: "end",
    followOnAppend: true,
    scrollEndThreshold: 120,
  });
  useLayoutEffect(() => {
    virtualRef.current = virtual;
    const pane = scroller.current;
    const release = () => {
      virtual.shouldAdjustScrollPositionOnItemSizeChange = undefined;
    };
    const gestures = ["wheel", "touchstart", "pointerdown", "keydown"];
    for (const gesture of gestures)
      pane?.addEventListener(gesture, release, { passive: true });
    return () => {
      for (const gesture of gestures)
        pane?.removeEventListener(gesture, release);
      release();
      virtualRef.current = null;
    };
  }, [virtual, scroller]);
  useEffect(() => {
    if (store.targetId) {
      const index = rows.findIndex((row) => row.id === store.targetId);
      if (index >= 0) virtual.scrollToIndex(index, { align: "center" });
    }
  }, [rows, store, virtual]);
  return (
    <div
      ref={content}
      onWheel={(event) => {
        if (event.deltaY < 0 && (scroller.current?.scrollTop ?? 0) < 80)
          store.earlier();
      }}
      onKeyDown={(event) => {
        if (
          event.key === "PageUp" &&
          scroller.current &&
          scroller.current.scrollTop < scroller.current.clientHeight
        )
          store.earlier();
      }}
    >
      {store.rootParent && <FocusedContext store={store} />}
      {(store.rootParent !== root || store.targetId) && (
        <button
          className="focus-ring text-xs"
          onClick={() => store.focus(root)}
        >
          Back to conversation
        </button>
      )}
      {search.error && (
        <p role="alert" className="text-ink-muted">
          {search.error.message}
        </p>
      )}
      {query.length >= 2 && (
        <section aria-label="Search results">
          {search.data?.hits.map((hit) => (
            <button
              key={hit.seq}
              className="block text-left text-xs focus-ring"
              onClick={() => void store.seek(hit.seq)}
            >
              {hit.excerpt}
            </button>
          ))}
        </section>
      )}
      {store.error && (
        <p role="alert" className="text-ink-muted">
          {store.error}
        </p>
      )}
      {store.ready &&
        rows.length === 0 &&
        !store.error &&
        !store.rootParent && (
          <p className="text-xs text-ink-muted">
            {turns.length
              ? "No transcript content has been saved for this turn."
              : "No turns yet"}
          </p>
        )}
      {!store.ready && !store.error && (
        <p role="status">Loading conversation...</p>
      )}
      {store.branches.get(store.rootParent)?.cursor && (
        <button className="focus-ring text-xs" onClick={() => store.earlier()}>
          Load earlier history
        </button>
      )}
      {store.branches.get(store.rootParent)?.detached && (
        <button
          className="focus-ring text-xs"
          onClick={() => store.focus(store.rootParent)}
        >
          Back to latest
        </button>
      )}
      <div
        ref={top}
        style={{ height: virtual.getTotalSize(), position: "relative" }}
      >
        {virtual.getVirtualItems().map((item) => (
          <div
            key={item.key}
            data-index={item.index}
            ref={virtual.measureElement}
            style={{
              position: "absolute",
              top: 0,
              width: "100%",
              transform: `translateY(${item.start - margin}px)`,
            }}
          >
            <NodeRow
              store={store}
              id={rows[item.index].id}
              depth={rows[item.index].depth}
              onReviewRequest={onReviewRequest}
              fidelity={fidelity}
              onResend={onResend}
              isLive={isLive}
              status={
                turns.find(
                  (entry) =>
                    entry.turn.id ===
                    store.nodes.get(rows[item.index].id)?.turnId,
                )?.turn.status
              }
              onOpenFile={onOpenFile}
            />
          </div>
        ))}
      </div>
      {root && store.rootParent === root && <TrajectoryReport store={store} />}
    </div>
  );
}

function TrajectoryReport({ store }: { store: ExecutionStore }) {
  const id = store.rootParent;
  const subscribe = useMemo(
    () => (fn: () => void) => store.subscribe(id, fn),
    [store, id],
  );
  const node = useSyncExternalStore(
    subscribe,
    () => store.nodes.get(id),
    () => store.nodes.get(id),
  );
  if (!node) return null;
  const report =
    node.trajectory ??
    (node.event.kind === "subagent" ? node.event : undefined);
  if (!report?.text) return null;
  return (
    <div className="py-4 text-base leading-relaxed">
      <EventBody
        event={report}
        bodyVersion={node.bodyRevision}
        bodyUrl={`/api/v1/sessions/${segment(store.sessionId)}/bodies/${report.bodyRef?.seq ?? report.seq}`}
      >
        {(event) => <Markdown text={event.text ?? ""} />}
      </EventBody>
    </div>
  );
}

function ExecutionSymbol({ node }: { node: ExecutionNode }) {
  const kind = node.event.toolSummary?.kind;
  const Icon =
    node.trajectory || node.event.kind === "subagent"
      ? BranchIcon
      : kind === "shell" || node.commands > 0
        ? TerminalIcon
        : kind === "read" || node.reads > 0
          ? FileTextIcon
          : kind === "edit" || kind === "write"
            ? PencilIcon
            : kind === "search"
              ? SearchIcon
              : node.event.kind === "thinking" || node.thinking > 0
                ? SparklesIcon
                : ToolIcon;
  return <Icon size={16} className="mt-0.5 shrink-0" />;
}

function FocusedContext({ store }: { store: ExecutionStore }) {
  const parent = store.rootParent;
  const subscribe = useMemo(
    () => (notify: () => void) => store.subscribe(parent, notify),
    [store, parent],
  );
  const node = useSyncExternalStore(
    subscribe,
    () => store.nodes.get(parent),
    () => store.nodes.get(parent),
  );
  if (!node) return null;
  return (
    <section
      aria-label="Focused execution"
      className="mb-3 border-b border-line pb-2"
    >
      {store.rootParent !== store.initialRoot && (
        <h3 className="text-sm font-medium">{label(node)}</h3>
      )}
      {node.running > 0 && (
        <p
          data-running-activity
          className="text-xs text-ink motion-safe:animate-[execution-activity-pulse_2.4s_ease-in-out_infinite]"
        >
          {node.activity || "Working"}
          {node.running > 1 ? ` · ${node.running - 1} more running` : ""}
        </p>
      )}
      {(node.trajectory ?? node.event).interiorState &&
        (node.children === 0 ||
          (node.trajectory ?? node.event).interiorState !== "full") && (
          <p className="text-xs text-ink-soft">
            {interiorMessage((node.trajectory ?? node.event).interiorState)}
          </p>
        )}
      {node.children === 0 &&
        !(node.trajectory ?? node.event).interiorState && (
          <p className="text-xs text-ink-muted">
            No child events were recorded.
          </p>
        )}
      {node.failed > 0 && (
        <p className="text-xs text-ink-muted">{node.failed} failed</p>
      )}
      {node.waiting > 0 && (
        <p className="text-xs text-warning">Waiting for input</p>
      )}
      {node.structureError && (
        <p className="text-xs text-warning">{node.structureError}</p>
      )}
      {node.event.kind !== "execution" && node.event.kind !== "subagent" && (
        <EventBody
          event={node.event}
          bodyVersion={node.bodyRevision}
          bodyUrl={`/api/v1/sessions/${segment(store.sessionId)}/nodes/${segment(node.id)}/body`}
        >
          {(event) =>
            event.kind === "tool_call" ? (
              <OutputBlock
                text={event.text || JSON.stringify(event.data ?? {})}
                maxLines={20}
              />
            ) : (
              <Markdown text={event.text ?? ""} />
            )
          }
        </EventBody>
      )}
    </section>
  );
}

const NodeRow = memo(function NodeRow({
  store,
  id,
  depth,
  onOpenFile,
  status,
  onResend,
  isLive,
  onReviewRequest,
  fidelity,
}: {
  store: ExecutionStore;
  id: string;
  depth: number;
  status?: string;
  onResend: (text: string) => void;
  isLive: boolean;
  onReviewRequest: (id: string) => void;
  fidelity: Fidelity;
  onOpenFile: (path: string, line?: number) => void;
}) {
  const subscribe = useMemo(
    () => (fn: () => void) => store.subscribe(id, fn),
    [store, id],
  );
  const snapshot = () =>
    `${store.nodes.get(id)?.revision}:${store.branches.get(id)?.open}:${store.branches.get(id)?.cursor}:${store.branches.get(id)?.detached}`;
  useSyncExternalStore(subscribe, snapshot, snapshot);
  const node = store.nodes.get(id);
  const [copyState, setCopyState] = useState("");
  async function copyTurn() {
    if (!node) return;
    setCopyState("Copying...");
    try {
      const { turns } = await api.sessions.exportTurn(
        store.sessionId,
        node.turnId,
      );
      const lines: string[] = [];
      for (const { turn, events } of turns) {
        if (turn.requestText) lines.push(`> ${turn.requestText}`);
        for (const preview of events) {
          const event = preview.bodyRef
            ? {
                ...preview,
                ...(await api.sessions.eventBody(
                  store.sessionId,
                  preview.bodyRef.seq,
                )),
              }
            : preview;
          if (event.kind === "assistant") lines.push(event.text ?? "");
          if (event.kind === "tool_call")
            lines.push(`- ${event.name ?? "tool"}: ${event.text ?? ""}`);
        }
      }
      await navigator.clipboard.writeText(lines.join("\n\n"));
      setCopyState("Copied");
    } catch (error) {
      setCopyState(error instanceof Error ? error.message : "Copy failed");
    }
  }
  const t = useT();
  const ui = useWorkspaceUI();
  const ownerSession = useSessionIdParam() ?? store.sessionId;

  const rowRef = useRef<HTMLElement>(null);
  const workStatus = node?.turn?.status;
  const open = store.branches.get(id)?.open ?? false;
  useEffect(() => {
    if (node?.event.kind !== "work") return;
    const terminal = [
      "completed",
      "cancelled",
      "interrupted",
      "failed",
    ].includes(workStatus ?? "");
    if (
      !store.observedStatus.has(id) &&
      !terminal &&
      !store.disclosureChoices.has(id)
    )
      store.open(id);
    if (
      store.observedStatus.has(id) &&
      !terminal &&
      !store.disclosureChoices.has(id) &&
      !store.branches.has(id)
    )
      store.open(id);
    if (
      terminal &&
      store.observedStatus.has(id) &&
      !["completed", "cancelled", "interrupted", "failed"].includes(
        store.observedStatus.get(id)!,
      ) &&
      !store.disclosureChoices.has(id)
    ) {
      const pane = rowRef.current?.closest<HTMLElement>("[data-scroller]");
      if (
        pane &&
        pane.scrollHeight - pane.scrollTop - pane.clientHeight < 120 &&
        !window.getSelection()?.toString()
      )
        store.close(id);
    }
    if (workStatus) store.observedStatus.set(id, workStatus);
  }, [node?.event.kind, workStatus, store, id]);
  const revealPanel = (tab: string) => {
    activatePanel(ownerSession, tab);
    if (ui.isMobile) ui.openDrawer("files");
    else ui.setRightOpen(true);
  };
  if (!node) return null;
  if (node.event.kind === "work") {
    const duration =
      fidelity === "stream" &&
      node.turn?.startedAtMs != null &&
      node.turn.completedAtMs != null &&
      node.turn.completedAtMs >= node.turn.startedAtMs
        ? node.turn.completedAtMs - node.turn.startedAtMs
        : undefined;
    const terminal = [
      "completed",
      "cancelled",
      "interrupted",
      "failed",
    ].includes(workStatus ?? "");
    const title =
      workStatus === "cancelled"
        ? t("reading.stopped")
        : workStatus === "interrupted"
          ? t("reading.interrupted")
          : workStatus === "failed"
            ? t("reading.failed")
            : workStatus === "stopping"
              ? t("reading.stopping")
              : workStatus === "settling"
                ? t("reading.settling")
                : !workStatus || workStatus === "unknown"
                  ? t("reading.workDetails")
                  : !terminal
                  ? t("reading.working")
                  : duration !== undefined
                    ? t("reading.worked", {
                        duration: formatDuration(duration),
                      })
                    : t("reading.workDetails");
    return (
      <section
        ref={rowRef}
        data-node-id={id}
        data-turn-id={node.turnId}
        data-turn-status={workStatus}
        className="reading-work"
      >
        <button
          type="button"
          aria-expanded={open}
          className="reading-summary focus-ring"
          onClick={() => {
            store.disclosureChoices.set(id, !open);
            if (open) store.close(id);
            else store.open(id);
          }}
        >
          <span>{title}</span>
          <ChevronRightIcon size={14} className={open ? "rotate-90" : ""} />
          {node.failed > 0 && (
            <span className="text-xs">
              {t("reading.failedCount", { count: node.failed })}
            </span>
          )}
          {node.running > 0 && terminal && (
            <span className="text-xs">
              {t("reading.stillWorking", { count: node.running })}
            </span>
          )}
        </button>
        {node.waiting > 0 && (
          <p className="text-sm text-ink-muted">{t("reading.waiting")}</p>
        )}
        {node.turn?.errorMessage && (
          <p role="alert" className="text-sm text-ink-muted">
            {node.turn.errorMessage}
          </p>
        )}
        {open && store.branches.get(id)?.cursor && (
          <button
            className="text-xs focus-ring"
            onClick={() => store.earlier(id)}
          >
            {t("reading.earlier")}
          </button>
        )}
      </section>
    );
  }
  const prose = ["user", "assistant", "status", "error", "usage"].includes(
    node.event.kind,
  );
  const report =
    node.trajectory ??
    (node.event.kind === "subagent" ? node.event : undefined);
  const timed = report ?? node.event;
  const elapsed =
    fidelity === "stream" &&
    timed.phase === "completed" &&
    timed.startedAtMs !== undefined &&
    timed.arrivalAtMs !== undefined &&
    timed.arrivalAtMs >= timed.startedAtMs
      ? timed.arrivalAtMs - timed.startedAtMs
      : undefined;
  const abandoned =
    (report?.data as { closedBy?: string })?.closedBy === "server";
  return (
    <section
      data-node-id={id}
      data-turn-id={node.turnId}
      data-turn-status={status}
      aria-label={
        report
          ? `${label(node)} delegation`
          : node.event.kind === "tool_call"
            ? `${node.event.name ?? "Tool"} tool call`
            : node.event.kind === "tool_result"
              ? "Tool result"
              : `Turn ${node.turnId}`
      }
      style={{ paddingLeft: Math.min(depth, 3) * 12 }}
      className={`reading-node py-2 ${node.event.kind === "user" ? "reading-input" : ""}`}
    >
      {prose ? (
        <EventBody
          event={node.event}
          bodyVersion={node.bodyRevision}
          bodyUrl={`/api/v1/sessions/${segment(store.sessionId)}/nodes/${segment(node.id)}/body`}
        >
          {(event) => (
            <div
              role={event.kind === "error" ? "alert" : undefined}
              className={
                event.kind === "error"
                  ? "text-danger"
                  : event.kind === "user"
                    ? "reading-bubble text-ink"
                    : node.readingRole === "progress"
                      ? "text-ink-soft"
                      : "text-ink"
              }
            >
              {event.kind === "usage" ? (
                <span className="text-3xs text-ink-muted">
                  {reportedUsage(event.data)}
                </span>
              ) : (
                <Markdown text={event.text ?? ""} />
              )}
            </div>
          )}
        </EventBody>
      ) : node.event.kind === "tool_result" ? (
        <EventBody
          event={node.event}
          bodyVersion={node.bodyRevision}
          bodyUrl={`/api/v1/sessions/${segment(store.sessionId)}/nodes/${segment(node.id)}/body`}
        >
          {(event) => (
            <div className="pl-4 text-xs">
              {node.failed > 0 && (
                <p className="text-ink-muted">
                  Failed
                  {typeof (event.data as { exitCode?: number })?.exitCode ===
                  "number"
                    ? ` (exit ${(event.data as { exitCode: number }).exitCode})`
                    : ""}
                </p>
              )}
              <OutputBlock text={event.text ?? ""} tone="neutral" />
            </div>
          )}
        </EventBody>
      ) : (
        <>
          <button
            type="button"
            aria-expanded={report ? undefined : open}
            onClick={() => {
              if (report) {
                const target = {
                  sessionId: store.sessionId,
                  nodeId: id,
                  name: label(node),
                };
                const panel = panelSnapshot(ownerSession);
                updatePanel(ownerSession, {
                  directory: false,
                  trail: store.initialRoot
                    ? [...panel.trail, target]
                    : [target],
                });
                revealPanel("subagents");
              } else if (open) store.close(id);
              else store.open(id);
            }}
            className="group flex w-full items-start gap-2 rounded py-1 text-left text-sm text-ink-muted hover:bg-hover focus-ring"
          >
            <ChevronRightIcon
              size={12}
              className={`mt-0.5 shrink-0 ${open ? "rotate-90" : ""}`}
            />
            <ExecutionSymbol node={node} />
            <span className="min-w-0 flex-1 break-words">
              <span
                title={label(node)}
                className={
                  node.event.kind === "tool_call" ? "block truncate" : ""
                }
              >
                {label(node)}
              </span>
              {elapsed !== undefined && node.event.kind !== "execution" && (
                <span className="ml-2">{formatDuration(elapsed)}</span>
              )}
              {node.running > 0 && (
                <span
                  data-running-activity
                  className="ml-2 text-ink motion-safe:animate-[execution-activity-pulse_2.4s_ease-in-out_infinite]"
                >
                  {node.activity || "Working"}
                  {node.running > 1
                    ? ` · ${node.running - 1} more running`
                    : ""}
                </span>
              )}
              {node.failed > 0 && (
                <span className="ml-2 font-medium">{node.failed} failed</span>
              )}
              {node.structureError && (
                <span className="block text-warning">
                  {node.structureError}
                </span>
              )}
              {abandoned && (
                <span className="ml-2 text-warning">never finished</span>
              )}
              {Boolean(node.unfinished) && (
                <span className="ml-2 text-warning">
                  {node.unfinished} unfinished
                </span>
              )}
              {node.waiting > 0 && (
                <span className="ml-2 font-medium">Waiting for input</span>
              )}
              {report?.text && (
                <span className="block line-clamp-2">{report.text}</span>
              )}
              {report?.interiorState && report.interiorState !== "full" && (
                <span className="block">
                  {interiorMessage(report.interiorState)}
                </span>
              )}
            </span>
          </button>
          {open && node.event.kind === "execution" && (
            <ToolWindow
              store={store}
              parent={id}
              fidelity={fidelity}
              onOpenFile={onOpenFile}
              onResend={onResend}
              isLive={isLive}
              onReviewRequest={onReviewRequest}
            />
          )}
          {open && report && node.trajectory && (
            <EventBody event={report}>
              {(event) => <Markdown text={event.text ?? ""} />}
            </EventBody>
          )}
          {open && node.waiting > 0 && node.event.kind === "request" && (
            <button
              className="focus-ring text-xs text-warning"
              onClick={() =>
                onReviewRequest(
                  (node.event.data as { requestId: string }).requestId,
                )
              }
            >
              Review request
            </button>
          )}
          {open && node.event.kind !== "execution" && (
            <div className="pl-4">
              <button
                type="button"
                className="focus-ring text-xs text-ink-muted underline"
                onClick={() => {
                  updatePanel(ownerSession, {
                    detail: { sessionId: store.sessionId, node },
                  });
                  revealPanel("detail");
                }}
              >
                {t("reading.openDetails")}
              </button>
              <EventBody
                event={node.event}
                bodyVersion={node.bodyRevision}
                bodyUrl={`/api/v1/sessions/${segment(store.sessionId)}/nodes/${segment(node.id)}/body`}
              >
                {(event) => {
                  const shape =
                    event.kind === "tool_call" ? describeTool(event) : null;
                  return (
                    <>
                      {shape?.path && (
                        <button
                          className="focus-ring font-mono text-xs"
                          onClick={() => onOpenFile(shape.path!)}
                        >
                          {shape.path}
                        </button>
                      )}
                      {shape?.hunks.map((h, index) => (
                        <DiffView
                          key={index}
                          before={h.before}
                          after={h.after}
                          path={h.path}
                          onOpenFile={onOpenFile}
                        />
                      ))}
                      {event.text &&
                        (event.kind === "tool_result" ? (
                          <OutputBlock text={event.text} tone="neutral" />
                        ) : (
                          <Markdown text={event.text} />
                        ))}
                      {event.data != null && (
                        <OutputBlock
                          text={JSON.stringify(event.data, null, 2)}
                          maxLines={30}
                        />
                      )}
                    </>
                  );
                }}
              </EventBody>
            </div>
          )}
          {open &&
            !report &&
            node.event.kind !== "execution" &&
            node.children > 0 && (
              <button
                className="focus-ring text-xs"
                onClick={() => store.focus(id)}
              >
                Focus this branch
              </button>
            )}
          {open && store.branches.get(id)?.cursor && (
            <button
              className="focus-ring text-xs"
              onClick={() => store.earlier(id)}
            >
              Load earlier items
            </button>
          )}
          {open && store.branches.get(id)?.detached && (
            <button
              className="focus-ring text-xs"
              onClick={() => store.focus(id)}
            >
              View latest items
            </button>
          )}
        </>
      )}
      {node.id.startsWith("p:") && (
        <div className="flex gap-2 text-3xs text-ink-muted">
          {status && <span>{status}</span>}
          <button
            className="focus-ring"
            disabled={copyState === "Copying..."}
            onClick={() => void copyTurn()}
          >
            {copyState || "Copy"}
          </button>
          {!isLive &&
            ["failed", "cancelled", "interrupted"].includes(status ?? "") && (
              <button
                className="focus-ring"
                onClick={() =>
                  void api.turns
                    .get(node.turnId)
                    .then(({ turn }) => onResend(turn.requestText))
                    .catch((error) => setCopyState(error.message))
                }
              >
                Resend
              </button>
            )}
        </div>
      )}
    </section>
  );
});

function ToolWindow({
  store,
  parent,
  ...props
}: {
  store: ExecutionStore;
  parent: string;
  fidelity: Fidelity;
  onOpenFile: (path: string, line?: number) => void;
  onResend: (text: string) => void;
  isLive: boolean;
  onReviewRequest: (id: string) => void;
}) {
  const t = useT();
  const scroller = useRef<HTMLDivElement>(null);
  const subscribe = useMemo(
    () => (fn: () => void) => store.subscribe("", fn),
    [store],
  );
  useSyncExternalStore(subscribe, store.snapshot, store.snapshot);
  const rows = store.rowsUnder(parent);
  useLayoutEffect(() => {
    if (scroller.current)
      scroller.current.scrollTop = store.groupScroll.get(parent) ?? 0;
  }, [store, parent]);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => 36,
    getItemKey: (index) => rows[index].id,
    overscan: 5,
    anchorTo: "end",
    followOnAppend: true,
    scrollEndThreshold: 80,
  });
  return (
    <div
      ref={scroller}
      className="reading-tools pane"
      tabIndex={0}
      aria-label={t("reading.toolCalls")}
      data-tool-window
      onScroll={(event) =>
        store.groupScroll.set(parent, event.currentTarget.scrollTop)
      }
    >
      {store.branches.get(parent)?.cursor && (
        <button
          className="text-xs focus-ring"
          onClick={() => store.earlier(parent)}
        >
          {t("reading.earlier")}
        </button>
      )}
      <div style={{ height: virtual.getTotalSize(), position: "relative" }}>
        {virtual.getVirtualItems().map((item) => (
          <div
            key={item.key}
            data-index={item.index}
            ref={virtual.measureElement}
            style={{
              position: "absolute",
              top: 0,
              width: "100%",
              transform: `translateY(${item.start}px)`,
            }}
          >
            <NodeRow
              store={store}
              id={rows[item.index].id}
              depth={rows[item.index].depth}
              {...props}
            />
          </div>
        ))}
      </div>
      {store.branches.get(parent)?.detached && (
        <button
          className="text-xs focus-ring"
          onClick={() => store.latest(parent)}
        >
          {t("reading.latest")}
        </button>
      )}
    </div>
  );
}
