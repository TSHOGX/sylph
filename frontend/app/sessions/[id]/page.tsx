"use client";

/**
 * A session: the agent-workspace conversation canvas.
 *
 * The transcript is one continuous timeline rather than a list of per-turn cards.
 * A turn boundary is a rule with a status line, not a container — an agent's work
 * reads as a single log, and boxing each turn re-introduces the chat framing that
 * `docs/agent-ui-design-analysis.md` argues against.
 *
 * Live and durable events come from different places and must not both render the
 * same turn: history holds every finished turn, and the SSE stream holds the one
 * in flight. The live turn is filtered out of history until the refetched history
 * actually contains it, which closes the gap where it would vanish for a frame.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import { useParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, type Turn, type TurnWithEvents } from "@/lib/api";
import { ErrorBoundary } from "@/lib/error-boundary";
import { formatTimestamp, sessionLabel } from "@/lib/format";
import { densityClasses, useSettings } from "@/lib/settings";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useSSE } from "@/lib/use-sse";
import { AttachmentPicker, type StagedAttachment } from "@/components/attachment-picker";
import { CommandDialog, type CommandInvocation } from "@/components/command-dialog";
import { Timeline } from "@/components/timeline";
import { OriginBadge, SessionStatusBadge, TurnStatusBadge } from "@/components/badges";

const TERMINAL: ReadonlySet<Turn["status"]> = new Set([
  "completed",
  "failed",
  "cancelled",
  "interrupted",
]);

export default function SessionPage() {
  const params = useParams();
  const sessionId = String(params.id);
  const queryClient = useQueryClient();
  const { settings } = useSettings();
  const { showArtifact, registerSessionActions } = useWorkspaceUI();

  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<StagedAttachment[]>([]);
  /** Turn started by this tab. Null means "whatever history says is running". */
  const [sentTurnId, setSentTurnId] = useState<string | null>(null);
  const [commandOpen, setCommandOpen] = useState(false);
  /** Global expand override from `Shift+E` / `Shift+C`. Null restores per-row
   *  defaults, so the timeline is not stuck open after one use. */
  const [expandAll, setExpandAll] = useState<boolean | null>(null);
  const [find, setFind] = useState<string | null>(null);

  const sessionQuery = useQuery({
    queryKey: ["session", sessionId],
    queryFn: () => api.sessions.get(sessionId),
  });

  const historyQuery = useQuery({
    queryKey: ["session", sessionId, "history"],
    queryFn: () => api.sessions.history(sessionId),
  });

  const session = sessionQuery.data?.session;
  const history = useMemo(() => historyQuery.data?.turns ?? [], [historyQuery.data]);

  // The turn to stream is derived, not stored: a turn this tab started takes
  // precedence, otherwise any unfinished turn in history is adopted so reopening
  // the page mid-run reattaches to the live stream.
  const adoptedTurnId = useMemo(
    () => history.find((entry) => !TERMINAL.has(entry.turn.status))?.turn.id ?? null,
    [history],
  );
  const watchedTurnId = sentTurnId ?? adoptedTurnId;

  const watchedTurn = useQuery({
    queryKey: ["turn", watchedTurnId],
    queryFn: () => api.turns.get(watchedTurnId!),
    enabled: Boolean(watchedTurnId),
    // Poll only while the turn is unfinished: the SSE channel carries events,
    // not turn state, so status is what tells us when to stop streaming.
    refetchInterval: (query) =>
      query.state.data && TERMINAL.has(query.state.data.turn.status) ? false : 1500,
  });

  const watchedStatus = watchedTurn.data?.turn.status;
  const finished = Boolean(watchedStatus && TERMINAL.has(watchedStatus));

  const absorbed = history.some((entry) => entry.turn.id === watchedTurnId);
  const liveTurnId = watchedTurnId && !(finished && absorbed) ? watchedTurnId : null;
  const isLive = Boolean(liveTurnId) && !finished;

  // Pull the finished turn into history. A genuine external-system sync.
  useEffect(() => {
    if (!finished) return;
    void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "history"] });
    void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
  }, [finished, queryClient, sessionId]);

  const sendTurn = useMutation({
    // `override` carries a resent turn's text. Its attachments are deliberately
    // not resent: they were staged from local files this tab no longer holds.
    mutationFn: (override?: string) =>
      api.turns.create(sessionId, {
        text: (override ?? text).trim(),
        attachments: override
          ? []
          : attachments.map(({ name, type, data }) => ({ name, type, data })),
        // A retried POST must not start a second run of the same message.
        idempotencyKey: crypto.randomUUID(),
      }),
    onSuccess: (result) => {
      setSentTurnId(result.turn.id);
      setText("");
      setAttachments([]);
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "history"] });
    },
  });

  const stopTurn = useMutation({
    mutationFn: () => api.turns.stop(liveTurnId!),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["turn", liveTurnId] });
    },
  });

  /** A command runs as a turn, so its output streams into the same transcript. */
  const runCommand = useMutation({
    mutationFn: (invocation: CommandInvocation) =>
      api.turns.create(sessionId, {
        command: invocation.command,
        args: invocation.args,
        idempotencyKey: crypto.randomUUID(),
      }),
    onSuccess: (result) => {
      setSentTurnId(result.turn.id);
      setCommandOpen(false);
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "history"] });
    },
  });

  const agentsQuery = useQuery({ queryKey: ["agents"], queryFn: () => api.agents.list() });
  const commands = useMemo(() => {
    const descriptor = agentsQuery.data?.agents.find((a) => a.name === session?.agent);
    return descriptor?.commands ?? {};
  }, [agentsQuery.data, session?.agent]);
  const hasCommands = Object.keys(commands).length > 0;

  const openFile = useCallback(
    (path: string, line?: number) => {
      // Absolute paths inside the workspace are rewritten to relative ones: the
      // file API is rooted at the session workdir and takes nothing else, and
      // agents report paths both ways.
      const workdir = session?.workdir.replace(/\/+$/, "");
      let relative = path;
      if (workdir && path.startsWith(`${workdir}/`)) {
        relative = path.slice(workdir.length + 1);
      }
      showArtifact({ sessionId, path: relative.replace(/^\/+/, ""), line });
    },
    [session?.workdir, sessionId, showArtifact],
  );

  // Session-scoped shortcuts are dispatched by the shell, which cannot know this
  // page's state, so the handler is registered here and unregistered on unmount.
  useEffect(
    () =>
      registerSessionActions((action) => {
        switch (action) {
          case "stopTurn":
            if (isLive && !stopTurn.isPending) stopTurn.mutate();
            return true;
          case "expandAll":
            setExpandAll(true);
            return true;
          case "collapseAll":
            setExpandAll(false);
            return true;
          case "findInSession":
            setFind((current) => (current === null ? "" : current));
            return true;
          default:
            return false;
        }
      }),
    [registerSessionActions, isLive, stopTurn],
  );

  if (sessionQuery.isPending) {
    return (
      <output
        aria-live="polite"
        className="flex flex-1 items-center justify-center text-sm text-gray-500"
      >
        Loading session...
      </output>
    );
  }

  if (sessionQuery.error || !session) {
    const error = sessionQuery.error;
    const missing = error instanceof ApiError && error.status === 404;
    return (
      <div role="alert" className="flex flex-1 items-center justify-center px-4">
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold text-red-800">
            {missing ? "Session not found" : "Could not load this session"}
          </h1>
          <p className="mt-2 text-sm text-gray-600">
            {missing
              ? "It may have been created against a different data directory."
              : error instanceof Error
                ? error.message
                : "Unknown error."}
          </p>
          <div className="mt-4 flex justify-center gap-2">
            {!missing && (
              <button
                type="button"
                onClick={() => void sessionQuery.refetch()}
                className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
              >
                Retry
              </button>
            )}
            <Link
              href="/"
              className="rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              All sessions
            </Link>
          </div>
        </div>
      </div>
    );
  }

  const isNative = session.origin === "native";
  const canSend = text.trim().length > 0 && !sendTurn.isPending && !isLive;
  const density = densityClasses(settings.density);

  return (
    <>
      <header className="shrink-0 border-b border-gray-200 px-4 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="truncate text-sm font-semibold">{sessionLabel(session)}</h1>
          <OriginBadge origin={session.origin} />
          <SessionStatusBadge status={session.status} />
          <div className="ml-auto flex items-center gap-1">
            <button
              type="button"
              onClick={() => setExpandAll(expandAll === true ? null : true)}
              className="rounded px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Expand all
            </button>
            <button
              type="button"
              onClick={() => setExpandAll(expandAll === false ? null : false)}
              className="rounded px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Collapse all
            </button>
          </div>
        </div>
        <p className="mt-0.5 truncate text-[11px] text-gray-500">
          {session.agent} · <span className="font-mono">{session.workdir}</span> · created{" "}
          {formatTimestamp(session.createdAtMs)}
        </p>
      </header>

      {find !== null && (
        <FindBar value={find} onChange={setFind} onClose={() => setFind(null)} />
      )}

      {isNative && (
        <aside
          className="shrink-0 border-b border-violet-200 bg-violet-50 px-4 py-1.5 text-[11px] text-violet-900"
          aria-label="Native session policy"
        >
          <span className="font-medium">Native session — identity preserved.</span> Sylph
          does not inject an identity, reset, or compact it, and prompts pass through
          unchanged.
        </aside>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        <div className={`mx-auto max-w-3xl ${density.text}`}>
          <ErrorBoundary label="the transcript">
            <Transcript
              history={history}
              isPending={historyQuery.isPending}
              error={historyQuery.error}
              onRetry={() => void historyQuery.refetch()}
              liveTurnId={liveTurnId}
              isLive={isLive}
              onResend={(value) => sendTurn.mutate(value)}
              onOpenFile={openFile}
              expandAll={expandAll}
              filter={find}
            />
          </ErrorBoundary>
        </div>
      </div>

      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (canSend) sendTurn.mutate(undefined);
        }}
        className="shrink-0 border-t border-gray-200 px-4 py-2"
      >
        <div className="mx-auto max-w-3xl">
          <label htmlFor="message" className="sr-only">
            Message
          </label>
          <textarea
            id="message"
            value={text}
            rows={2}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              // Enter sends; Shift+Enter and Cmd/Ctrl+Enter insert a newline.
              if (event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.ctrlKey) {
                event.preventDefault();
                if (canSend) sendTurn.mutate(undefined);
              }
            }}
            placeholder={
              isLive ? "Waiting for the current turn to finish..." : "Send a message..."
            }
            disabled={isLive || sendTurn.isPending}
            className="w-full resize-y rounded-lg border border-gray-200 px-3 py-2 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:bg-gray-50"
          />

          <div className="mt-1.5">
            <AttachmentPicker
              attachments={attachments}
              onChange={setAttachments}
              disabled={isLive || sendTurn.isPending}
            />
          </div>

          {sendTurn.error && (
            <p role="alert" className="mt-1.5 text-xs text-red-700">
              {describeSendError(sendTurn.error)}
            </p>
          )}
          {runCommand.error && (
            <p role="alert" className="mt-1.5 text-xs text-red-700">
              {describeSendError(runCommand.error)}
            </p>
          )}

          <div className="mt-1.5 flex items-center justify-between gap-3">
            <p className="text-[10px] text-gray-500">
              Enter to send · Shift+Enter for a new line
            </p>
            <div className="flex gap-1.5">
              {hasCommands && (
                <button
                  type="button"
                  onClick={() => setCommandOpen(true)}
                  disabled={isLive || sendTurn.isPending}
                  className="rounded-md border border-gray-200 px-3 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
                >
                  Command
                </button>
              )}
              {isLive && (
                <button
                  type="button"
                  onClick={() => stopTurn.mutate()}
                  disabled={stopTurn.isPending}
                  className="rounded-md border border-red-300 px-3 py-1 text-xs font-medium text-red-800 hover:bg-red-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600 disabled:opacity-50"
                >
                  {stopTurn.isPending ? "Stopping..." : "Stop turn"}
                </button>
              )}
              <button
                type="submit"
                disabled={!canSend}
                className="rounded-md bg-gray-900 px-4 py-1 text-xs font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
              >
                {sendTurn.isPending ? "Sending..." : "Send"}
              </button>
            </div>
          </div>
        </div>
      </form>

      {commandOpen && (
        <CommandDialog
          commands={commands}
          busy={runCommand.isPending}
          onClose={() => setCommandOpen(false)}
          onSubmit={(invocation) => runCommand.mutate(invocation)}
        />
      )}
    </>
  );
}

/**
 * In-session filter (`Cmd+F`).
 *
 * Filters turns rather than highlighting matches inside them. Highlighting would
 * need to reach into every renderer — Markdown, diffs, code blocks — and rewrite
 * their text nodes; filtering answers the actual question ("which turn was that
 * in?") without that coupling. The browser's own find still works on what remains.
 */
function FindBar({
  value,
  onChange,
  onClose,
}: {
  value: string;
  onChange: (value: string) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => ref.current?.focus(), []);

  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-gray-200 bg-gray-50 px-4 py-1">
      <label htmlFor="find-in-session" className="text-[10px] font-medium text-gray-500">
        Filter turns
      </label>
      <input
        id="find-in-session"
        ref={ref}
        type="search"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            onClose();
          }
        }}
        placeholder="Text in a prompt or reply..."
        className="flex-1 rounded border border-gray-200 bg-white px-2 py-0.5 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
      />
      <button
        type="button"
        onClick={onClose}
        className="rounded px-1.5 text-[11px] text-gray-500 hover:bg-gray-200 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
      >
        Close
      </button>
    </div>
  );
}

/** Turn an API failure into text that names the actual cause. */
function describeSendError(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "SESSION_ALREADY_RUNNING":
        return "This session already has a turn in flight. Wait for it to finish, or stop it first.";
      case "ATTACHMENT_INVALID":
        return `Attachment rejected: ${error.message}`;
      case "PAYLOAD_TOO_LARGE":
        return "The message plus attachments exceed the 10 MB per-turn limit. Remove a file and try again.";
      default:
        return error.message;
    }
  }
  return error instanceof Error ? error.message : "Could not send the message.";
}

/**
 * Flatten a turn into Markdown for the clipboard.
 *
 * Tool calls are included as headings now, unlike the Phase 4 version which kept
 * only prose: in an agent transcript the tool calls are the record of what
 * happened, so a copy without them is not the conversation.
 */
function turnAsText(entry: TurnWithEvents): string {
  const lines = entry.turn.requestText ? [`> ${entry.turn.requestText}`, ""] : [];
  for (const event of entry.events) {
    if (event.kind === "assistant" && event.text) {
      lines.push(event.text, "");
    } else if (event.kind === "tool_call") {
      lines.push(`- ${event.name ?? "tool"}: ${(event.text ?? "").split("\n")[0]}`);
    }
  }
  return lines.join("\n").trim();
}

function Transcript({
  history,
  isPending,
  error,
  onRetry,
  liveTurnId,
  isLive,
  onResend,
  onOpenFile,
  expandAll,
  filter,
}: {
  history: TurnWithEvents[];
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
  liveTurnId: string | null;
  isLive: boolean;
  onResend: (text: string) => void;
  onOpenFile: (path: string, line?: number) => void;
  expandAll: boolean | null;
  filter: string | null;
}) {
  const past = useMemo(() => {
    const rows = history.filter((entry) => entry.turn.id !== liveTurnId);
    const needle = filter?.trim().toLowerCase();
    if (!needle) return rows;
    return rows.filter(
      (entry) =>
        entry.turn.requestText.toLowerCase().includes(needle) ||
        entry.events.some((event) => event.text?.toLowerCase().includes(needle)),
    );
  }, [history, liveTurnId, filter]);

  if (isPending) {
    return (
      <output aria-live="polite" className="block py-8 text-center text-sm text-gray-500">
        Loading conversation...
      </output>
    );
  }

  if (error) {
    return (
      <div role="alert" className="rounded-lg border border-red-200 bg-red-50 p-4">
        <p className="text-sm font-medium text-red-900">
          Could not load the conversation history.
        </p>
        <p className="mt-1 text-sm text-red-800">
          {error instanceof Error ? error.message : "Unknown error."}
        </p>
        <button
          type="button"
          onClick={onRetry}
          className="mt-3 rounded border border-red-300 bg-white px-3 py-1.5 text-sm font-medium hover:bg-red-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600"
        >
          Retry
        </button>
      </div>
    );
  }

  const filtering = Boolean(filter?.trim());
  const empty = past.length === 0 && !liveTurnId;

  return (
    <div>
      {empty && !filtering && (
        <div className="py-12 text-center">
          <p className="text-sm font-medium text-gray-700">No turns yet</p>
          <p className="mt-1 text-sm text-gray-500">
            Send a message below to start the conversation.
          </p>
        </div>
      )}
      {empty && filtering && (
        <p className="py-12 text-center text-sm text-gray-500">No turns match that text.</p>
      )}

      {past.map((entry) => (
        <TurnSection
          key={entry.turn.id}
          entry={entry}
          onResend={onResend}
          disabled={isLive}
          onOpenFile={onOpenFile}
          expandAll={expandAll}
        />
      ))}

      {liveTurnId && !filtering && (
        <LiveTurn turnId={liveTurnId} isLive={isLive} onOpenFile={onOpenFile} expandAll={expandAll} />
      )}
    </div>
  );
}

/**
 * One completed turn.
 *
 * The boundary is a rule with a status line above the events, not a box around
 * them — enough to see where a turn began without fragmenting the log.
 */
function TurnSection({
  entry,
  onResend,
  disabled,
  onOpenFile,
  expandAll,
}: {
  entry: TurnWithEvents;
  onResend: (text: string) => void;
  disabled: boolean;
  onOpenFile: (path: string, line?: number) => void;
  expandAll: boolean | null;
}) {
  const [copied, setCopied] = useState(false);

  // Resending a turn that already succeeded would duplicate work the agent has
  // done, so it is offered only where the turn produced no usable result.
  const resendable =
    entry.turn.status === "failed" ||
    entry.turn.status === "cancelled" ||
    entry.turn.status === "interrupted";

  const duration =
    entry.turn.completedAtMs && entry.turn.startedAtMs
      ? (entry.turn.completedAtMs - entry.turn.startedAtMs) / 1000
      : undefined;

  async function copy() {
    try {
      await navigator.clipboard.writeText(turnAsText(entry));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard access can be refused; a silent success would be misleading.
      setCopied(false);
    }
  }

  return (
    <section aria-label={`Turn ${entry.turn.id}`} className="border-t border-gray-100 pt-1 first:border-t-0">
      <Timeline events={entry.events} onOpenFile={onOpenFile} expandAll={expandAll} />

      {entry.turn.errorMessage && (
        <p role="alert" className="mt-1 text-xs text-red-700">
          {entry.turn.errorMessage}
        </p>
      )}

      <div className="flex items-center gap-2 py-1 text-[10px] text-gray-500">
        <TurnStatusBadge status={entry.turn.status} />
        {duration !== undefined && <span>{duration.toFixed(1)}s</span>}
        <button
          type="button"
          onClick={() => void copy()}
          className="rounded px-1 hover:bg-gray-100 hover:text-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          {copied ? "Copied" : "Copy"}
        </button>
        {resendable && entry.turn.requestText && (
          <button
            type="button"
            onClick={() => onResend(entry.turn.requestText)}
            disabled={disabled}
            className="rounded px-1 hover:bg-gray-100 hover:text-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
          >
            Resend
          </button>
        )}
      </div>
    </section>
  );
}

function LiveTurn({
  turnId,
  isLive,
  onOpenFile,
  expandAll,
}: {
  turnId: string;
  isLive: boolean;
  onOpenFile: (path: string, line?: number) => void;
  expandAll: boolean | null;
}) {
  const { events, status } = useSSE({ turnId, enabled: isLive });
  const anchorRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);

  // Scroll anchoring: follow the stream only while the user is already at the
  // bottom, so scrolling up to read earlier output is not yanked away.
  useEffect(() => {
    const node = anchorRef.current;
    if (!node) return;
    const scroller = node.closest(".overflow-y-auto");
    if (!scroller) return;
    const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    pinnedRef.current = distance < 120;
    if (pinnedRef.current) node.scrollIntoView({ block: "end" });
  }, [events]);

  return (
    <section aria-label="Current turn" className="border-t border-gray-100 pt-1">
      <Timeline events={events} onOpenFile={onOpenFile} expandAll={expandAll} />

      <div className="flex items-center gap-1.5 py-1 text-[10px]">
        <span
          aria-hidden="true"
          className={`h-1.5 w-1.5 rounded-full ${
            status === "open"
              ? "animate-pulse bg-blue-500"
              : status === "reconnecting"
                ? "bg-amber-500"
                : "bg-gray-300"
          }`}
        />
        <span aria-live="polite" className="text-gray-500">
          {status === "open"
            ? events.length === 0
              ? "Waiting for the agent to respond..."
              : "Streaming"
            : status === "reconnecting"
              ? "Connection dropped — reconnecting..."
              : status === "connecting"
                ? "Connecting..."
                : "Stream closed"}
        </span>
      </div>
      <div ref={anchorRef} />
    </section>
  );
}
