"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import { useParams } from "next/navigation";
import { useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, type Turn, type TurnWithEvents } from "@/lib/api";
import { ErrorBoundary } from "@/lib/error-boundary";
import { formatTimestamp, sessionLabel } from "@/lib/format";
import { useSSE } from "@/lib/use-sse";
import { AttachmentPicker, type StagedAttachment } from "@/components/attachment-picker";
import { CommandDialog, type CommandInvocation } from "@/components/command-dialog";
import { EventList } from "@/components/event-list";
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

  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<StagedAttachment[]>([]);
  /** Turn started by this tab. Null means "whatever history says is running". */
  const [sentTurnId, setSentTurnId] = useState<string | null>(null);
  const [commandOpen, setCommandOpen] = useState(false);

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
  // precedence, otherwise any unfinished turn in history is adopted so
  // reopening the page mid-run reattaches to the live stream.
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

  // A finished turn belongs to durable history. It keeps its live pane until
  // the refetched history actually contains it, which closes the gap where it
  // would otherwise vanish for one render.
  const absorbed = history.some((entry) => entry.turn.id === watchedTurnId);
  const liveTurnId = watchedTurnId && !(finished && absorbed) ? watchedTurnId : null;
  const isLive = Boolean(liveTurnId) && !finished;

  // Pull the finished turn into history. This is a genuine external-system
  // sync — the cache needs invalidating — and sets no local state.
  useEffect(() => {
    if (!finished) return;
    void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "history"] });
    void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
  }, [finished, queryClient, sessionId]);

  const sendTurn = useMutation({
    mutationFn: () =>
      api.turns.create(sessionId, {
        text: text.trim(),
        attachments: attachments.map(({ name, type, data }) => ({ name, type, data })),
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

  /**
   * A command runs as a turn, so it reuses the same lifecycle: the runner
   * receives the invocation and its output streams into the transcript.
   */
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

  // Commands come from the agent descriptor, so an agent that declares none
  // gets no button rather than an empty dialog.
  const agentsQuery = useQuery({
    queryKey: ["agents"],
    queryFn: () => api.agents.list(),
  });
  const commands = useMemo(() => {
    const descriptor = agentsQuery.data?.agents.find((a) => a.name === session?.agent);
    return descriptor?.commands ?? {};
  }, [agentsQuery.data, session?.agent]);
  const hasCommands = Object.keys(commands).length > 0;

  if (sessionQuery.isPending) {
    return (
      <output
        aria-live="polite"
        className="flex min-h-screen items-center justify-center text-sm text-gray-500"
      >
        Loading session...
      </output>
    );
  }

  if (sessionQuery.error || !session) {
    const error = sessionQuery.error;
    const missing = error instanceof ApiError && error.status === 404;
    return (
      <div role="alert" className="flex min-h-screen items-center justify-center px-4">
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

  return (
    <div className="flex min-h-screen flex-col bg-white">
      <header className="border-b">
        <div className="container mx-auto px-4 py-3">
          <Link
            href="/"
            className="text-sm text-gray-500 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            ← All sessions
          </Link>
          <div className="mt-1.5 flex flex-wrap items-center gap-2">
            <h1 className="text-xl font-semibold">{sessionLabel(session)}</h1>
            <OriginBadge origin={session.origin} />
            <SessionStatusBadge status={session.status} />
          </div>
          <p className="mt-1 text-xs text-gray-500">
            {session.agent} · <span className="font-mono">{session.workdir}</span> ·
            created {formatTimestamp(session.createdAtMs)}
          </p>
        </div>
      </header>

      <main className="container mx-auto flex w-full max-w-4xl flex-1 flex-col px-4 py-6">
        {isNative && (
          <aside className="mb-4 rounded-lg border border-violet-200 bg-violet-50 p-3 text-sm text-violet-900">
            <p className="font-medium">Native session — identity preserved</p>
            <p className="mt-1 text-violet-800">
              This session originated in {session.agent}&apos;s own storage. Sylph does
              not inject an identity, reset, or compact it, and prompts are passed
              through unchanged.
            </p>
          </aside>
        )}

        <ErrorBoundary label="the transcript">
          <Transcript
            history={history}
            isPending={historyQuery.isPending}
            error={historyQuery.error}
            onRetry={() => void historyQuery.refetch()}
            liveTurnId={liveTurnId}
            isLive={isLive}
          />
        </ErrorBoundary>

        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (canSend) sendTurn.mutate();
          }}
          className="mt-4 border-t pt-4"
        >
          <label htmlFor="message" className="sr-only">
            Message
          </label>
          <textarea
            id="message"
            value={text}
            rows={3}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              // Enter sends; Shift+Enter and Cmd/Ctrl+Enter insert a newline,
              // matching the convention of chat-style composers.
              if (e.key === "Enter" && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
                e.preventDefault();
                if (canSend) sendTurn.mutate();
              }
            }}
            placeholder={
              isLive ? "Waiting for the current turn to finish..." : "Send a message..."
            }
            disabled={isLive || sendTurn.isPending}
            className="w-full resize-y rounded-lg border px-3 py-2 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:bg-gray-50"
          />

          <div className="mt-2">
            <AttachmentPicker
              attachments={attachments}
              onChange={setAttachments}
              disabled={isLive || sendTurn.isPending}
            />
          </div>

          {sendTurn.error && (
            <p role="alert" className="mt-2 text-sm text-red-700">
              {describeSendError(sendTurn.error)}
            </p>
          )}

          <div className="mt-3 flex items-center justify-between gap-3">
            <p className="text-xs text-gray-500">
              Enter to send · Shift+Enter for a new line
            </p>
            <div className="flex gap-2">
              {hasCommands && (
                <button
                  type="button"
                  onClick={() => setCommandOpen(true)}
                  disabled={isLive || sendTurn.isPending}
                  className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
                >
                  Command
                </button>
              )}
              {isLive && (
                <button
                  type="button"
                  onClick={() => stopTurn.mutate()}
                  disabled={stopTurn.isPending}
                  className="rounded-lg border border-red-300 px-4 py-2 text-sm font-medium text-red-800 hover:bg-red-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600 disabled:opacity-50"
                >
                  {stopTurn.isPending ? "Stopping..." : "Stop turn"}
                </button>
              )}
              <button
                type="submit"
                disabled={!canSend}
                className="rounded-lg bg-gray-900 px-5 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
              >
                {sendTurn.isPending ? "Sending..." : "Send"}
              </button>
            </div>
          </div>
        </form>

        {runCommand.error && (
          <p role="alert" className="mt-2 text-sm text-red-700">
            {describeSendError(runCommand.error)}
          </p>
        )}

        {commandOpen && (
          <CommandDialog
            commands={commands}
            busy={runCommand.isPending}
            onClose={() => setCommandOpen(false)}
            onSubmit={(invocation) => runCommand.mutate(invocation)}
          />
        )}
      </main>
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

function Transcript({
  history,
  isPending,
  error,
  onRetry,
  liveTurnId,
  isLive,
}: {
  history: TurnWithEvents[];
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
  liveTurnId: string | null;
  isLive: boolean;
}) {
  const bottomRef = useRef<HTMLDivElement>(null);

  // History already contains every finished turn. Rendering the live turn from
  // the same list too would duplicate it, so it is filtered out here and drawn
  // by LiveTurn instead.
  const past = useMemo(
    () => history.filter((entry) => entry.turn.id !== liveTurnId),
    [history, liveTurnId],
  );

  if (isPending) {
    return (
      <output aria-live="polite" className="py-8 text-center text-sm text-gray-500">
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

  const empty = past.length === 0 && !liveTurnId;

  return (
    <div className="flex-1 space-y-6 overflow-y-auto">
      {empty && (
        <div className="py-12 text-center">
          <p className="text-sm font-medium text-gray-700">No turns yet</p>
          <p className="mt-1 text-sm text-gray-500">
            Send a message below to start the conversation.
          </p>
        </div>
      )}

      {past.map((entry) => (
        <section key={entry.turn.id} aria-label={`Turn ${entry.turn.id}`}>
          <div className="mb-1.5 flex items-center gap-2">
            <TurnStatusBadge status={entry.turn.status} />
            <span className="text-xs text-gray-400">
              {formatTimestamp(entry.turn.createdAtMs)}
            </span>
          </div>
          {entry.turn.errorMessage && (
            <p role="alert" className="mb-2 text-sm text-red-700">
              {entry.turn.errorMessage}
            </p>
          )}
          <EventList events={entry.events} />
        </section>
      ))}

      {liveTurnId && <LiveTurn turnId={liveTurnId} isLive={isLive} />}

      <div ref={bottomRef} />
    </div>
  );
}

function LiveTurn({ turnId, isLive }: { turnId: string; isLive: boolean }) {
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
    const distance =
      scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    pinnedRef.current = distance < 120;
    if (pinnedRef.current) node.scrollIntoView({ block: "end" });
  }, [events]);

  return (
    <section aria-label="Current turn">
      <div className="mb-1.5 flex items-center gap-2 text-xs">
        <span
          aria-hidden="true"
          className={`h-2 w-2 rounded-full ${
            status === "open"
              ? "animate-pulse bg-blue-500"
              : status === "reconnecting"
                ? "bg-amber-500"
                : "bg-gray-300"
          }`}
        />
        <span aria-live="polite" className="text-gray-500">
          {status === "open"
            ? "Streaming"
            : status === "reconnecting"
              ? "Connection dropped — reconnecting..."
              : status === "connecting"
                ? "Connecting..."
                : "Stream closed"}
        </span>
      </div>

      {events.length === 0 && isLive && (
        <p className="text-sm text-gray-500">Waiting for the agent to respond...</p>
      )}

      <EventList events={events} />
      <div ref={anchorRef} />
    </section>
  );
}
