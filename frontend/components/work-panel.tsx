"use client";
import { artifactPath } from "@/lib/artifact-path";

import { useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import dynamic from "next/dynamic";
const EmbeddedSession = dynamic(() =>
  import("./session-view").then((module) => module.SessionView),
);
import type { ExecutionNode } from "@/lib/execution-store";
import { api, segment } from "@/lib/api";
import type { Fidelity, InteriorState } from "@/lib/api-types";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useSessionIdParam } from "@/lib/use-session-id-param";
import {
  activatePanel,
  updatePanel,
  usePanelState,
  type TrajectoryTarget,
} from "@/lib/work-panel-state";
import { useT } from "@/lib/i18n";
import { readResource } from "@/lib/reading-resource";
import { ArtifactsPane } from "./artifacts-pane";
import { ExecutionTimeline } from "./execution-timeline";
import { ApprovalDialog } from "./approval-dialog";
import { EventBody } from "./event-body";
import { OutputBlock } from "./code-block";
import { DiffView } from "./diff-view";
import { describeTool } from "@/lib/tool-shape";
import { Sheet } from "./ui/sheet";
import { FolderIcon, SparklesIcon, XIcon, MaximizeIcon } from "./icons";
import { useDialog } from "@/lib/use-dialog";

const subscribeWindow = (fn: () => void) => {
  window.addEventListener("resize", fn);
  window.addEventListener("storage", fn);
  return () => {
    window.removeEventListener("resize", fn);
    window.removeEventListener("storage", fn);
  };
};

interface Delegate {
  id: string;
  target: TrajectoryTarget;
  name: string | null;
  status: string;
  interiorState?: InteriorState;
  fidelity: Fidelity;
  workdir: string;
  parentId: string | null;
  parentName?: string | null;
}
interface DirectoryPage {
  items: Delegate[];
  counts: { active: number; finished: number; unknown: number };
  nextCursor: string | null;
  hasMore: boolean;
}

export function WorkPanel() {
  const sessionId = useSessionIdParam() ?? "";
  const state = usePanelState(sessionId);
  const ui = useWorkspaceUI();
  const t = useT();
  const available = useSyncExternalStore(
    subscribeWindow,
    () => window.innerWidth,
    () => 1440,
  );
  const savedWidth = useSyncExternalStore(
    subscribeWindow,
    () => Number(localStorage.getItem("sylph.workPanelWidth")) || null,
    () => null,
  );
  const tablist = useRef<HTMLDivElement>(null);
  const minimum = state.active === "files" ? 320 : 420;
  const desired =
    state.width ?? savedWidth ?? (state.active === "files" ? 384 : 480);
  const remaining = available - (!ui.isMobile && ui.leftOpen ? 240 : 0);
  const sheet = ui.isMobile && state.active === "files" && !state.maximized;
  const overlay = state.maximized || ui.isMobile || remaining < minimum + 480;
  const visible = ui.isMobile ? ui.drawer === "files" : ui.rightOpen;
  const width = Math.max(minimum, Math.min(desired, remaining - 480));
  const close = () => {
    updatePanel(sessionId, { maximized: false });
    if (ui.isMobile) ui.closeDrawer();
    else ui.setRightOpen(false);
  };
  // Keep the same panel tree during maximize/restore. Dialog behavior is enabled
  // only for its overlay presentation, so desktop reading never traps focus.
  const { ref: panelRef, onKeyDown: dialogKeyDown } = useDialog<HTMLElement>({
    onClose: close,
    enabled: visible && overlay && !sheet,
  });
  const setWidth = (value: number) => {
    const next = Math.max(minimum, Math.min(value, remaining - 480));
    updatePanel(sessionId, { width: next });
    localStorage.setItem("sylph.workPanelWidth", String(next));
  };
  const label = (tab: string) =>
    t(
      tab === "files"
        ? "reading.files"
        : tab === "subagents"
          ? "reading.subagents"
          : "reading.details",
    );
  const closeTab = (tab: string) => {
    const tabs = state.tabs.filter((value) => value !== tab);
    const next =
      tabs[Math.min(state.tabs.indexOf(tab), tabs.length - 1)] ?? "files";
    updatePanel(sessionId, {
      tabs,
      active: state.active === tab ? next : state.active,
    });
    if (!tabs.length) close();
    else
      requestAnimationFrame(() =>
        document.getElementById(`work-tab-${next}`)?.focus(),
      );
  };
  if (!visible) return null;
  const target = state.trail.at(-1);
  const content = (
    <aside
      ref={panelRef}
      onKeyDown={dialogKeyDown}
      tabIndex={-1}
      aria-label={t("reading.panel")}
      role={overlay && !sheet ? "dialog" : "complementary"}
      aria-modal={(overlay && !sheet) || undefined}
      className={`work-panel flex min-h-0 shrink-0 flex-col bg-canvas text-ink ${sheet ? "h-full w-full" : overlay ? "fixed inset-y-0 right-0 z-40 shadow-xl" : "relative border-l border-line"}`}
      style={{
        width: sheet
          ? "100%"
          : overlay
            ? ui.isMobile || state.maximized
              ? "100%"
              : "min(640px, 95vw)"
            : width,
      }}
    >
      {!overlay && (
        <div
          role="separator"
          tabIndex={0}
          aria-label={t("reading.resize")}
          aria-orientation="vertical"
          aria-valuemin={minimum}
          aria-valuemax={Math.max(minimum, remaining - 480)}
          aria-valuenow={width}
          className="absolute inset-y-0 -left-1 z-10 w-2 cursor-col-resize focus-ring"
          onKeyDown={(event) => {
            if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
              event.preventDefault();
              setWidth(width + (event.key === "ArrowLeft" ? 32 : -32));
            }
          }}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
          }}
          onPointerMove={(event) => {
            if (event.currentTarget.hasPointerCapture(event.pointerId))
              setWidth(available - event.clientX);
          }}
        />
      )}
      <div className="flex shrink-0 items-center gap-1 border-b border-line p-2">
        <div
          ref={tablist}
          role="tablist"
          aria-label={t("reading.panel")}
          className="flex min-w-0 flex-1 gap-1 overflow-x-auto"
          onKeyDown={(event) => {
            const tabs = Array.from(
              tablist.current?.querySelectorAll<HTMLButtonElement>(
                '[role="tab"]',
              ) ?? [],
            );
            const index = tabs.indexOf(
              document.activeElement as HTMLButtonElement,
            );
            const next =
              event.key === "ArrowRight"
                ? (index + 1) % tabs.length
                : event.key === "ArrowLeft"
                  ? (index - 1 + tabs.length) % tabs.length
                  : event.key === "Home"
                    ? 0
                    : event.key === "End"
                      ? tabs.length - 1
                      : -1;
            if (next >= 0) {
              event.preventDefault();
              tabs[next]?.focus();
            }
            if (event.key === "Delete" && index >= 0) {
              event.preventDefault();
              closeTab(state.tabs[index]);
            }
          }}
        >
          {state.tabs.map((tab) => (
            <div
              key={tab}
              className={`flex items-center rounded-lg ${tab === state.active ? "bg-sunken" : ""}`}
            >
              <button
                id={`work-tab-${tab}`}
                role="tab"
                aria-selected={tab === state.active}
                aria-controls={`work-content-${tab}`}
                tabIndex={tab === state.active ? 0 : -1}
                className="whitespace-nowrap rounded-lg px-3 py-2 text-sm focus-ring"
                onClick={() => activatePanel(sessionId, tab)}
              >
                {label(tab)}
              </button>
            </div>
          ))}
        </div>
        <button
          className="rounded p-1 tap-target text-ink-muted focus-ring"
          aria-label={t("reading.closeTab", { name: label(state.active) })}
          onClick={() => closeTab(state.active)}
        >
          <XIcon size={14} />
        </button>
        {!state.tabs.includes("files") && (
          <button
            className="p-1 tap-target focus-ring"
            aria-label={t("reading.files")}
            onClick={() => activatePanel(sessionId, "files")}
          >
            <FolderIcon size={16} />
          </button>
        )}
        {!state.tabs.includes("subagents") && (
          <button
            className="p-1 tap-target focus-ring"
            aria-label={t("reading.subagents")}
            onClick={() => activatePanel(sessionId, "subagents")}
          >
            <SparklesIcon size={16} />
          </button>
        )}
        <button
          className="p-2 tap-target focus-ring"
          aria-label={t(
            state.maximized ? "reading.restore" : "reading.maximize",
          )}
          onClick={() =>
            updatePanel(sessionId, { maximized: !state.maximized })
          }
        >
          <MaximizeIcon size={16} />
        </button>
        <button
          className="p-2 tap-target focus-ring"
          aria-label={t("reading.closePanel")}
          onClick={close}
        >
          <XIcon size={14} />
        </button>
      </div>
      {state.tabs.map((tab) => (
        <div
          key={tab}
          id={`work-content-${tab}`}
          role="tabpanel"
          aria-labelledby={`work-tab-${tab}`}
          hidden={state.active !== tab}
          className={
            state.active === tab ? "flex min-h-0 flex-1 flex-col" : "hidden"
          }
        >
          {tab === "files" && state.active === tab && <ArtifactsPane />}
          {tab === "subagents" &&
            state.active === tab &&
            (state.directory || !target ? (
              <DelegateDirectory owner={sessionId} />
            ) : (
              <>
                <div className="flex shrink-0 items-center gap-2 border-b border-line px-4 py-3 text-sm">
                  {state.trail.length > 1 && (
                    <button
                      className="focus-ring"
                      onClick={() =>
                        updatePanel(sessionId, {
                          trail: state.trail.slice(0, -1),
                        })
                      }
                    >
                      {t("reading.back")}
                    </button>
                  )}
                  <button
                    className="focus-ring text-ink-muted"
                    onClick={() => updatePanel(sessionId, { directory: true })}
                  >
                    ← {t("reading.allAgents")}
                  </button>
                  <span className="min-w-0 flex-1 truncate">
                    {target.name || t("reading.subagents")}
                  </span>
                  {!target.nodeId && (
                    <Link
                      className="focus-ring text-xs"
                      href={`/sessions/${segment(target.sessionId)}`}
                    >
                      {t("reading.openMain")}
                    </Link>
                  )}
                </div>
                {target.nodeId ? (
                  <TrajectoryReader
                    key={`${target.sessionId}/${target.nodeId}`}
                    owner={sessionId}
                    target={target}
                  />
                ) : (
                  <EmbeddedSession
                    key={target.sessionId}
                    sessionId={target.sessionId}
                    embedded
                  />
                )}
              </>
            ))}
          {tab === "detail" && state.active === tab && state.detail && (
            <ToolDetail owner={sessionId} />
          )}
        </div>
      ))}
    </aside>
  );
  return sheet ? (
    <Sheet title={t("reading.files")} onClose={close}>
      {content}
    </Sheet>
  ) : (
    content
  );
}

function DelegateDirectory({ owner }: { owner: string }) {
  const state = usePanelState(owner);
  const t = useT();
  const scroller = useRef<HTMLDivElement>(null);
  const [cursors, setCursors] = useState<string[]>([]);
  const cursor = cursors.at(-1);
  const query = useQuery({
    queryKey: ["session", owner, "delegations", state.search, cursor],
    queryFn: ({ signal }) =>
      readResource<DirectoryPage>(
        `/api/v1/sessions/${segment(owner)}/delegations?${new URLSearchParams({ q: state.search, ...(cursor ? { cursor } : {}) })}`,
        signal,
      ),
    refetchInterval: 1000,
  });
  useLayoutEffect(() => {
    if (scroller.current) scroller.current.scrollTop = state.directoryScroll;
  }, [state.directoryScroll]);
  const statusLabel = (value: string) =>
    value === "completed"
      ? t("reading.completed")
      : value === "running"
        ? t("reading.working")
        : value === "waiting"
          ? t("reading.waiting")
          : value === "failed"
            ? t("reading.failed")
            : value === "interrupted"
              ? t("reading.interrupted")
              : value === "cancelled"
                ? t("reading.stopped")
                : value === "stopping"
                  ? t("reading.stopping")
                  : value === "settling"
                    ? t("reading.settling")
                    : value === "queued"
                      ? t("reading.queued")
                      : value === "unfinished"
                        ? t("reading.unfinished")
                        : t("reading.unknown");
  return (
    <>
      <div className="p-4">
        <input
          type="search"
          aria-label={t("reading.searchAgents")}
          placeholder={t("reading.searchAgents")}
          value={state.search}
          onChange={(event) => {
            setCursors([]);
            updatePanel(owner, {
              search: event.target.value,
              directoryScroll: 0,
            });
          }}
          className="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm focus-ring"
        />
      </div>
      <div
        ref={scroller}
        className="pane min-h-0 flex-1 px-4 pb-6"
        onScroll={(event) => {
          updatePanel(owner, {
            directoryScroll: event.currentTarget.scrollTop,
          });
        }}
      >
        {query.isPending && <p role="status">{t("reading.loading")}</p>}
        {query.error && (
          <div role="alert">
            {query.error.message}
            <button className="focus-ring" onClick={() => void query.refetch()}>
              {t("reading.retry")}
            </button>
          </div>
        )}
        {(["active", "finished", "unknown"] as const).map((group) => {
          const items =
            query.data?.items.filter((item) =>
              group === "active"
                ? [
                    "running",
                    "waiting",
                    "queued",
                    "stopping",
                    "settling",
                  ].includes(item.status)
                : group === "unknown"
                  ? item.status === "unknown"
                  : ![
                      "running",
                      "waiting",
                      "queued",
                      "stopping",
                      "settling",
                      "unknown",
                    ].includes(item.status),
            ) ?? [];
          return (
            <section
              key={group}
              className="mb-6"
              aria-label={t(`reading.${group}`)}
            >
              <h3 className="mb-2 text-sm text-ink-muted">
                {t(`reading.${group}`)} · {query.data?.counts[group] ?? 0}
              </h3>
              {items.map((item) => (
                <button
                  key={item.id}
                  className="flex w-full items-start gap-3 rounded-lg px-2 py-3 text-left hover:bg-hover focus-ring"
                  onClick={() =>
                    updatePanel(owner, {
                      trail: [{ ...item.target, name: item.name }],
                      directory: false,
                    })
                  }
                >
                  <span className="text-ink-muted" aria-hidden>
                    ◉
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block break-words text-sm">
                      {item.name || t("reading.subagents")}
                    </span>
                    {item.parentName && (
                      <span className="block truncate text-xs text-ink-muted">
                        {item.parentName}
                      </span>
                    )}
                  </span>
                  <span className="text-xs text-ink-muted">
                    {statusLabel(item.status)}
                  </span>
                </button>
              ))}
            </section>
          );
        })}
        {query.data && query.data.items.length === 0 && (
          <p className="text-sm text-ink-muted">{t("reading.noAgents")}</p>
        )}
        <div className="flex gap-4 text-sm">
          <button
            disabled={!cursors.length}
            className="focus-ring disabled:opacity-40"
            onClick={() => setCursors(cursors.slice(0, -1))}
          >
            {t("reading.back")}
          </button>
          {query.data?.nextCursor && (
            <button
              className="focus-ring"
              onClick={() => setCursors([...cursors, query.data!.nextCursor!])}
            >
              {t("reading.more")}
            </button>
          )}
        </div>
      </div>
    </>
  );
}

function TrajectoryReader({
  owner,
  target,
}: {
  owner: string;
  target: TrajectoryTarget;
}) {
  const t = useT();
  const ui = useWorkspaceUI();
  const scroller = useRef<HTMLDivElement>(null);
  const state = usePanelState(owner);
  const key = `${target.sessionId}/${target.nodeId ?? ""}`;
  const queryClient = useQueryClient();
  const [requestId, setRequestId] = useState<string | null>(null);
  const requests = useQuery({
    queryKey: ["session", target.sessionId, "requests"],
    queryFn: ({ signal }) => api.sessions.requests(target.sessionId, signal),
    refetchInterval: 1000,
  });
  const request = requests.data?.find(
    (item) => item.id === requestId && item.status === "pending",
  );
  const answer = useMutation({
    mutationFn: ({
      id,
      option,
      input,
    }: {
      id: string;
      option: string;
      input?: unknown;
    }) => api.requests.resolve(id, option, input),
    onSettled: () => {
      void queryClient.invalidateQueries({
        queryKey: ["session", target.sessionId, "requests"],
      });
    },
  });
  const query = useQuery({
    queryKey: ["session", target.sessionId],
    queryFn: ({ signal }) => api.sessions.get(target.sessionId, signal),
    refetchInterval: 1000,
  });
  useLayoutEffect(() => {
    if (scroller.current) scroller.current.scrollTop = state.scrolls[key] ?? 0;
  }, [key, state.scrolls]);
  if (query.isPending)
    return (
      <p role="status" className="p-4">
        {t("reading.loading")}
      </p>
    );
  if (query.error || !query.data)
    return (
      <p role="alert" className="p-4">
        {query.error?.message}
      </p>
    );
  const session = query.data.session;
  const openFile = (path: string, line?: number) => {
    ui.showArtifact({
      sessionId: session.id,
      path: artifactPath(session.workdir, path),
      line,
    });
  };
  return (
    <>
      {requests.data
        ?.filter((item) => item.status === "pending")
        .map((item) => (
          <button
            key={item.id}
            className="shrink-0 px-4 py-2 text-left text-sm focus-ring"
            onClick={() => setRequestId(item.id)}
          >
            {t("reading.waiting")}
          </button>
        ))}
      {request && (
        <ApprovalDialog
          request={request}
          busy={answer.isPending}
          onDismiss={() => setRequestId(null)}
          onResolve={(option, input) =>
            answer.mutate({ id: request.id, option, input })
          }
        />
      )}
      {answer.error && <p role="alert">{answer.error.message}</p>}
      <div
        ref={scroller}
        data-scroller
        tabIndex={0}
        aria-label={target.name || t("reading.subagents")}
        className="pane min-h-0 flex-1 px-5 py-4"
        style={{ overflowAnchor: "none" }}
        onScroll={(event) =>
          updatePanel(owner, {
            scrolls: { ...state.scrolls, [key]: event.currentTarget.scrollTop },
          })
        }
      >
        <ExecutionTimeline
          view="panel"
          sessionId={session.id}
          root={target.nodeId ?? ""}
          scroller={scroller}
          fidelity={session.fidelity}
          collapseEpoch={0}
          onOpenFile={openFile}
          turns={[]}
          isLive={session.status === "running"}
          onResend={() => {}}
          onReviewRequest={setRequestId}
        />
      </div>
    </>
  );
}

function ToolDetail({ owner }: { owner: string }) {
  const { detail } = usePanelState(owner);
  const ui = useWorkspaceUI();
  const session = useQuery({
    queryKey: ["session", detail?.sessionId],
    queryFn: ({ signal }) => api.sessions.get(detail!.sessionId, signal),
    enabled: Boolean(detail),
  });
  const results = useQuery({
    queryKey: ["session", detail?.sessionId, "tool-results", detail?.node.id],
    queryFn: ({ signal }) =>
      readResource<{ nodes: ExecutionNode[] }>(
        `/api/v1/sessions/${segment(detail!.sessionId)}/nodes?${new URLSearchParams({ parent: detail!.node.id })}`,
        signal,
      ),
    enabled: Boolean(detail),
  });
  if (!detail) return null;
  const { node, sessionId } = detail;
  return (
    <div className="pane min-h-0 flex-1 p-4">
      <EventBody
        initiallyRequested
        event={node.event}
        bodyVersion={node.bodyRevision}
        bodyUrl={`/api/v1/sessions/${segment(sessionId)}/nodes/${segment(node.id)}/body`}
      >
        {(event) => {
          const shape = describeTool(event);
          return (
            <>
              <h3 className="mb-3 text-sm font-medium">{shape.name}</h3>
              {shape.hunks.map((h, i) => (
                <DiffView
                  key={i}
                  before={h.before}
                  after={h.after}
                  path={h.path}
                  onOpenFile={(path) =>
                    ui.showArtifact({
                      sessionId,
                      path: artifactPath(session.data?.session.workdir, path),
                    })
                  }
                />
              ))}
              <OutputBlock
                text={event.text || JSON.stringify(event.data, null, 2)}
                maxLines={200}
              />
            </>
          );
        }}
      </EventBody>
      {results.error && <p role="alert">{results.error.message}</p>}
      {results.data?.nodes
        .filter((result) => result.event.kind === "tool_result")
        .map((result) => (
          <EventBody
            key={result.id}
            initiallyRequested
            event={result.event}
            bodyVersion={result.bodyRevision}
            bodyUrl={`/api/v1/sessions/${segment(sessionId)}/nodes/${segment(result.id)}/body`}
          >
            {(event) => <OutputBlock text={event.text ?? ""} maxLines={200} />}
          </EventBody>
        ))}
    </div>
  );
}
