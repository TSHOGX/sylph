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
import { useVirtualizer } from "@tanstack/react-virtual";
import { formatDuration } from "@/lib/format";
import type { Fidelity } from "@/lib/api-types";
import { api, segment } from "@/lib/api";
import { ExecutionStore, reportedUsage, type ExecutionNode } from "@/lib/execution-store";
import { Markdown } from "./markdown";
import { EventBody } from "./event-body";
import { ChevronRightIcon } from "./icons";
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
}: {
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
  const store = useMemo(() => new ExecutionStore(sessionId), [sessionId]);
  const subscribe = useMemo(
    () => (fn: () => void) => store.subscribe("", fn),
    [store],
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
    return () => store.stop();
  }, [store]);
  useEffect(() => {
    store.active = isLive;
  }, [store, isLive]);
  useEffect(() => store.collapseAll(), [store, collapseEpoch]);
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
      {(store.rootParent || store.targetId) && (
        <button className="focus-ring text-xs" onClick={() => store.focus("")}>
          Back to conversation
        </button>
      )}
      {search.error && (
        <p role="alert" className="text-danger">
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
        <p role="alert" className="text-danger">
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
    </div>
  );
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
      <h3 className="text-sm font-medium">{label(node)}</h3>
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
        <p className="text-xs text-danger">{node.failed} failed</p>
      )}
      {node.waiting > 0 && (
        <p className="text-xs text-warning">Waiting for input</p>
      )}
      {node.structureError && (
        <p className="text-xs text-warning">{node.structureError}</p>
      )}
      {node.event.kind !== "execution" && (
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
    `${store.nodes.get(id)?.revision}:${store.branches.get(id)?.open}:${store.branches.get(id)?.cursor}`;
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
  if (!node) return null;
  const open = store.branches.get(id)?.open ?? false;
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
  const attention =
    node.failed > 0 ||
    node.waiting > 0 ||
    report?.interiorState === "unreadable";
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
      className="py-1"
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
                    ? "font-medium text-ink"
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
        <EventBody event={node.event} bodyVersion={node.bodyRevision}>
          {(event) => (
            <div className="pl-4 text-xs">
              {node.failed > 0 && (
                <p className="text-danger">
                  Failed
                  {typeof (event.data as { exitCode?: number })?.exitCode ===
                  "number"
                    ? ` (exit ${(event.data as { exitCode: number }).exitCode})`
                    : ""}
                </p>
              )}
              <OutputBlock
                text={event.text ?? ""}
                tone={node.failed ? "error" : "neutral"}
              />
            </div>
          )}
        </EventBody>
      ) : (
        <>
          <button
            type="button"
            aria-expanded={open}
            onClick={() => (open ? store.close(id) : store.open(id))}
            className={`group flex w-full items-start gap-1 rounded px-1 py-1 text-left text-xs focus-ring ${node.failed > 0 || report?.interiorState === "unreadable" ? "text-danger bg-danger-bg" : attention ? "text-warning bg-warning-bg" : "text-ink-soft bg-hover"}`}
          >
            <ChevronRightIcon
              size={12}
              className={`mt-0.5 shrink-0 ${open ? "rotate-90" : "opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100"}`}
            />
            <span className="min-w-0 break-words">
              <span>{label(node)}</span>
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
                          <OutputBlock
                            text={event.text}
                            tone={node.failed ? "error" : "neutral"}
                          />
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
          {open && node.children > 0 && (
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
