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
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  ApiError,
  type Facet,
  type Fidelity,
  type Turn,
  type TurnWithEvents,
  type UnavailableReason,
} from "@/lib/api";
import { useSessionIdParam } from "@/lib/use-session-id-param";
import { useT } from "@/lib/i18n";
import { ErrorBoundary } from "@/lib/error-boundary";
import { formatTimestamp, sessionLabel } from "@/lib/format";
import { densityClasses, useSettings } from "@/lib/settings";
import { relayName, useEnvironments } from "@/lib/environments";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useSSE } from "@/lib/use-sse";
import { useAgentSession } from "@/lib/use-agent-session";
import { ApprovalDialog } from "@/components/approval-dialog";
import { type StagedAttachment } from "@/components/attachment-picker";
import { Composer } from "@/components/composer";
import { Timeline } from "@/components/timeline";
import {
  EnvironmentChip,
  FidelityMarker,
  SessionStatusBadge,
  TurnStatusBadge,
} from "@/components/badges";
import { Button, IconButton } from "@/components/ui/button";
import { MenuButton } from "@/components/ui/menu";
import {
  CopyIcon,
  MoreIcon,
  RefreshIcon,
} from "@/components/icons";

const TERMINAL: ReadonlySet<Turn["status"]> = new Set([
  "completed",
  "failed",
  "cancelled",
  "interrupted",
]);

export default function SessionPage() {
  const t = useT();
  // Decoded by the hook, never read raw: the App Router hands back the raw path
  // segment, and a session id is the identity triple, so it is percent-encoded there.
  // `use-session-id-param` holds the reasoning and the three ways this failed.
  const sessionId = useSessionIdParam() ?? "";
  const queryClient = useQueryClient();
  const { settings } = useSettings();
  const { showArtifact, registerSessionActions, setActiveWorkdir } = useWorkspaceUI();

  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<StagedAttachment[]>([]);
  /** Turn started by this tab. Null means "whatever history says is running". */
  const [sentTurnId, setSentTurnId] = useState<string | null>(null);
  /** Global expand override from `Shift+E` / `Shift+C`. Null restores per-row
   *  defaults, so the timeline is not stuck open after one use. */
  const [expandAll, setExpandAll] = useState<boolean | null>(null);
  const [find, setFind] = useState<string | null>(null);
  const hasCommittedWork = useRef(false);

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

  // There is deliberately no empty-session cleanup here any more. `POST /sessions`
  // starts an agent rather than reserving a row, so a session has no identity until an
  // agent has assigned one — the empty-draft state this discarded cannot exist, and
  // the `DELETE /sessions/{id}` it called is gone from the API with it. What it cost
  // while it existed is worth recording, because it looked like three lines and was
  // not: three refs mirrored in an effect, a deferred timer so Strict Mode's
  // setup/cleanup probe could cancel it, and an unmount path that deleted a session
  // whenever any of its conditions read stale.

  // The turn to stream is derived, not stored: a turn this tab started takes
  // precedence, otherwise any unfinished turn in history is adopted so reopening
  // the page mid-run reattaches to the live stream.
  const adoptedTurnId = useMemo(
    () => history.find((entry) => !TERMINAL.has(entry.turn.status))?.turn.id ?? null,
    [history],
  );

  // This tab's claim expires once history holds its turn as terminal.
  //
  // `sentTurnId` has to outrank adoption while a turn this tab started is in flight,
  // and must stop doing so the moment it is not: an *agent-initiated* turn is adopted
  // from history, so a claim that never expires means the agent's own turn is never
  // streamed — it appears only on the next reload, which is how a delivered
  // notification still reads as a missing one.
  //
  // Derived rather than cleared in an effect. Reading it out of history is also what
  // keeps the handover gapless: the claim ends exactly when history can render the
  // turn instead, so there is no frame where it is neither live nor in history.
  const sentIsSettled = useMemo(
    () =>
      Boolean(
        sentTurnId &&
          history.some(
            (entry) => entry.turn.id === sentTurnId && TERMINAL.has(entry.turn.status),
          ),
      ),
    [sentTurnId, history],
  );
  const watchedTurnId = (sentIsSettled ? null : sentTurnId) ?? adoptedTurnId;

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
      hasCommittedWork.current = true;
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
   * A prompt command runs as a turn, so its output streams into the same transcript.
   *
   * Only a `prompt` command reaches here. A `control` command resolves against typed
   * session config and never becomes a turn — the composer routes on `kind`.
   */
  const runCommand = useMutation({
    mutationFn: ({ command, args }: { command: string; args: Record<string, string> }) =>
      api.turns.create(sessionId, {
        command,
        args,
        idempotencyKey: crypto.randomUUID(),
      }),
    onSuccess: (result) => {
      hasCommittedWork.current = true;
      setSentTurnId(result.turn.id);
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "history"] });
    },
  });

  // Scoped to *this session's* environment, because agent availability is per
  // machine: a global list would offer the composer's agent switcher an agent this
  // session's machine cannot run. `undefined` while the query is in flight, and for an
  // environment this client has not been told about — both of which the composer reads
  // as "no alternatives to offer" rather than as an empty machine.
  const environments = useEnvironments();
  const environment = environments.find(session?.environment ?? "");

  // The session's live agent: its real command list, and anything it is blocked
  // on. Owned by its own hook because both are session facts rather than turn
  // facts - an approval outlives the turn that raised it, and a capability list
  // changes while no turn is running.
  const agent = useAgentSession(sessionId);
  // Dismissing hides the panel without answering, so the request stays open and
  // the agent stays blocked. Keyed by request id, or dismissing one approval
  // would hide the next one too.
  const [dismissedRequest, setDismissedRequest] = useState<string | null>(null);
  const pendingRequest =
    agent.current && agent.current.id !== dismissedRequest ? agent.current : undefined;

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

  // The open session establishes which workspace `Cmd+Shift+K` browses. Without
  // this the history browser would fall back to the most recently used workspace,
  // which is the wrong one whenever the user navigated to an older session.
  const openWorkdir = sessionQuery.data?.session.workdir;
  useEffect(() => {
    if (openWorkdir) setActiveWorkdir(openWorkdir);
  }, [openWorkdir, setActiveWorkdir]);

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
        className="flex flex-1 items-center justify-center text-sm text-ink-muted"
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
          <h1 className="text-lg font-semibold text-danger">
            {missing ? "Session not found" : "Could not load this session"}
          </h1>
          <p className="mt-2 text-sm text-ink-muted">
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
                className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-sunken focus-ring"
              >
                Retry
              </button>
            )}
            <Link
              href="/"
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-ink hover:bg-accent-hover focus-ring"
            >
              Start screen
            </Link>
          </div>
        </div>
      </div>
    );
  }

  const density = densityClasses(settings.density);

  return (
    <>
      {/*
        `flex-nowrap` with a truncating title, not `flex-wrap`. Wrapping let the
        badges and the two buttons drop onto a second and third line on a phone, so
        the header grew to three rows and took a third of the viewport before the
        conversation started. Now the title yields and everything else holds its size.
      */}
      <header className="shrink-0 border-b border-line px-3 py-2 md:px-4">
        <div className="flex items-center gap-2">
          <h1 className="min-w-0 flex-1 truncate text-sm font-semibold">
            {sessionLabel(session)}
          </h1>
          {/* Only the exceptional facts. A `stream` record and the only machine are
              the ordinary cases and get no marker, for the reason the deleted origin
              badge got wrong: on a phone these compete with the title, so a marker
              that is on every session is pure noise. The status badge already says
              "running". */}
          <FidelityMarker value={session.fidelity} />
          {environments.showEnvironment && (
            <EnvironmentChip
              name={session.environmentName}
              reachableVia={relayName(session.environment, environments)}
            />
          )}
          {session.status === "running" && <SessionStatusBadge status={session.status} />}
          <div className="hidden shrink-0 items-center gap-1 md:flex">
            <Button variant="ghost" onClick={() => setExpandAll(expandAll === true ? null : true)}>
              Expand all
            </Button>
            <Button
              variant="ghost"
              onClick={() => setExpandAll(expandAll === false ? null : false)}
            >
              Collapse all
            </Button>
          </div>
          {/* The same two actions plus the ones that had no home, folded into one
              button on a phone. `Cmd+F` and `Shift+E`/`Shift+C` reach three of these
              on a desktop, and a phone has no modifier keys at all — so without this
              they were unreachable rather than merely inconvenient. */}
          <div className="shrink-0 md:hidden">
            <MenuButton
              label="Session actions"
              align="end"
              items={[
                { label: "Expand all blocks", onSelect: () => setExpandAll(true) },
                { label: "Collapse all blocks", onSelect: () => setExpandAll(false) },
                {
                  label: "Filter turns...",
                  separated: true,
                  onSelect: () => setFind((current) => (current === null ? "" : current)),
                },
                {
                  label: "Copy workspace path",
                  icon: <CopyIcon size={14} />,
                  onSelect: () =>
                    void navigator.clipboard.writeText(session.workdir).catch(() => {}),
                },
                {
                  label: "Refresh transcript",
                  icon: <RefreshIcon size={14} />,
                  onSelect: () => {
                    void queryClient.invalidateQueries({
                      queryKey: ["session", sessionId, "history"],
                    });
                  },
                },
              ]}
            >
              {(trigger) => (
                <IconButton label="Session actions" size={20} {...trigger}>
                  <MoreIcon size={20} />
                </IconButton>
              )}
            </MenuButton>
          </div>
        </div>
        {/*
          The path is the session's identity, so it stays — but it is elided from the
          *start*. An absolute path truncated at the end shows
          `/Users/hsw/Workspace/proj…` for every session under a home directory, which
          is the half identical across all of them; the leaf is the half that
          identifies it.

          `dir="rtl"` is how that is done, and the inner `<bdi>` is not optional.
          Without it the bidi algorithm reorders the leading separator to the trailing
          edge: `/tmp` renders as `tmp/`, and a long path as `…/sylph-web/frontend/`.
          `<bdi>` isolates the path so it keeps its own direction while the container
          still elides at the start. (`unicode-bidi: plaintext` fixes the short case
          but reverts to end-truncation, so it does not work here — both were checked
          in a browser.)

          The creation time is desktop-only: the least useful field here, and the first
          thing worth spending on a narrow line.
        */}
        <p className="mt-0.5 flex items-baseline gap-1 text-2xs text-ink-muted">
          <span className="shrink-0">{session.agent}</span>
          <span aria-hidden="true">·</span>
          <span
            dir="rtl"
            title={session.workdir}
            className="min-w-0 flex-1 truncate text-left font-mono"
          >
            <bdi>{session.workdir}</bdi>
          </span>
          <span className="hidden shrink-0 md:inline">
            · created {formatTimestamp(session.createdAtMs)}
          </span>
        </p>
      </header>

      {find !== null && (
        <FindBar value={find} onChange={setFind} onClose={() => setFind(null)} />
      )}

      {/* What Sylph can and cannot do with this session right now, when that is not
          "everything". This replaces the native-session policy banner, which promised
          Sylph would not reset or compact a session — a promise kept by construction,
          since nothing ever reset anything, so it described an absence as a feature on
          every one of those sessions forever. This says something that can actually
          change and that the user can sometimes act on. */}
      <DriveState control={agent.control} connecting={agent.connecting} />

      {/*
        `data-scroller` rather than being found by its utility class. `LiveTurn` needs
        this node to decide whether the user is pinned to the bottom, and it used to
        locate it with `closest(".overflow-y-auto")` — which silently breaks the
        moment a scroll container is expressed differently, and did break when this
        became the `pane` utility. An explicit contract cannot drift.
      */}
      <div data-scroller className="pane min-h-0 flex-1 px-3 py-3 md:px-4">
        <div className={`mx-auto max-w-3xl ${density.text}`}>
          <ErrorBoundary label="the transcript">
            <Transcript
              history={history}
              fidelity={session.fidelity}
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

      {/* A lost race means the panel on screen was already answered — by another
          frontend, or another tab. Worth saying: the user pressed a button and
          nothing happened, and the reason is not their mistake. `staleAnswer` was
          computed for this and had no surface. */}
      {agent.staleAnswer && (
        <p role="status" className="border-t border-line bg-canvas px-3 pt-2 text-2xs text-ink-muted">
          {t("approval.stale")}
        </p>
      )}

      {/* More than one request can be open at once — a dismissed approval stays
          pending and the agent stays blocked — so the count is stated rather than
          leaving the others invisible behind the first. Only while a panel is
          showing: once one is dismissed the composer reports the whole backlog, and
          two counts of the same thing in one column is worse than either. */}
      {pendingRequest && agent.requests.length > 1 && (
        <p className="border-t border-line bg-canvas px-3 pt-2 text-2xs text-ink-muted">
          {t("approval.pendingCount", { count: agent.requests.length })}
        </p>
      )}

      {pendingRequest && (
        <div className="border-t border-line bg-canvas px-3 pt-3">
          <ApprovalDialog
            request={pendingRequest}
            busy={agent.resolving}
            onResolve={(option, input) => {
              agent.resolve(option, input);
              setDismissedRequest(null);
            }}
            onDismiss={() => setDismissedRequest(pendingRequest.id)}
          />
        </div>
      )}

      <Composer
        session={session}
        agents={environment?.agents ?? []}
        // Drives the agent lock: an empty session can still change runner, one
        // with history cannot.
        turnCount={history.length}
        isLive={isLive}
        text={text}
        onTextChange={setText}
        attachments={attachments}
        onAttachmentsChange={setAttachments}
        onSend={() => sendTurn.mutate(undefined)}
        sending={sendTurn.isPending}
        sendError={sendTurn.error ?? runCommand.error}
        onStop={() => stopTurn.mutate()}
        stopping={stopTurn.isPending}
        // The agent's own list, read from its live handshake. A detached
        // session reports none, which is why the menu simply does not open
        // rather than offering a command that may no longer exist.
        commands={agent.link?.capabilities?.commands ?? []}
        // Passed so `/` can explain itself instead of doing nothing. An empty
        // command list has several causes needing different responses, and the
        // composer cannot tell them apart from the list alone — an empty surface
        // means "Sylph cannot reach it", never "there is nothing there".
        agentConnecting={agent.connecting}
        control={agent.control}
        onReattach={() => agent.reattach()}
        // Typed config from the live agent, which is what a control command like
        // `/model` resolves against instead of being sent as prompt text.
        config={agent.link?.capabilities?.config ?? []}
        onRunCommand={(name, rest) =>
          runCommand.mutate({ command: name, args: rest ? { args: rest } : {} })
        }
        onSetConfig={(configId, value) => agent.setConfig(configId, value)}
        // A refused set is reported rather than dropped: without this the chip keeps
        // its old value and the only symptom is that choosing a model did nothing.
        configError={agent.configError}
        // Gated on what the agent declared rather than offered unconditionally: an
        // interrupt button on an agent with no interrupt is a promise the handshake
        // already told us we cannot keep. `pi` supports both; a minimal adapter
        // may support neither.
        canInterrupt={agent.can("interrupt")}
        onInterrupt={() => agent.interrupt()}
        canSteer={agent.can("steer")}
        // The running turn is named as a precondition, so a steer that races the
        // turn boundary is refused rather than delivered into the next conversation.
        onSteer={(value) => agent.steer(value, liveTurnId ?? undefined)}
        // Dismissing hides a panel without answering, so the agent stays blocked.
        // Reported here because with the panel gone nothing else on screen would
        // say so, and a blocked session that merely looks idle leaves the user
        // waiting for an agent that is waiting for them.
        blockedCount={pendingRequest ? 0 : agent.requests.length}
        onShowBlocked={() => setDismissedRequest(null)}
      />

    </>
  );
}

/**
 * What Sylph cannot do with this session, and why — when there is something to say.
 *
 * # Why the notice is about `control` alone
 *
 * The two facets fail for different reasons and only one of them is worth a banner. An
 * unavailable `observe` means there is nothing to read, which the transcript states by
 * being empty; an unavailable `control` means the transcript reads fine and the
 * composer cannot send, and *that* is the state a user misreads as the app being
 * broken. So this names the half that is surprising.
 *
 * # Why it renders nothing while connecting
 *
 * A handshake spawns an agent CLI and takes seconds, during which `control` reads
 * unavailable because nothing has reached the machine yet. Announcing "this machine is
 * offline" for that window would be wrong on every session open, so `connecting`
 * outranks the reason — the composer's own status is where an in-flight handshake is
 * reported, because that is where the user is looking.
 *
 * # Why `role="status"` and not `alert`
 *
 * None of these is an error. A session running in a terminal is working exactly as
 * intended and simply is not ours to drive; a sleeping laptop is a machine to wake.
 * `alert` interrupts a screen reader mid-sentence, which is for something gone wrong.
 */
function DriveState({ control, connecting }: { control: Facet; connecting: boolean }) {
  if (control.available || connecting || !control.reason) return null;

  // Exhaustive over the closed union, so a reason added to the protocol is a compile
  // error here rather than a session silently rendering no explanation.
  const said: Record<UnavailableReason, { headline: string; detail: string }> = {
    environment_unreachable: {
      headline: "That machine is not connected.",
      detail:
        "History reads from Sylph's own record, so the transcript is complete. Nothing can be sent until the machine is back.",
    },
    not_ours_to_drive: {
      headline: "Running in a terminal.",
      detail:
        "This agent offers no channel to a second party, so Sylph can follow the session but cannot send to it.",
    },
    no_agent: {
      headline: "That agent is not installed on this machine.",
      detail:
        "History reads normally. Installing the agent's CLI there is what makes the session continuable.",
    },
    handshake_failed: {
      headline: "The agent would not start.",
      detail: "History reads normally. Sending a message will try again.",
    },
  };
  const { headline, detail } = said[control.reason];

  return (
    <aside
      role="status"
      aria-label="What Sylph can do with this session"
      className="shrink-0 border-b border-line bg-sunken px-3 py-1.5 text-2xs text-ink-soft md:px-4"
    >
      <span className="font-medium">{headline}</span>
      {/* Desktop-only detail, matching the header's own treatment: the headline
          carries what matters and the detail is two lines on a phone. It is a
          `<span>` rather than a second block so a screen reader reads one sentence
          — the notice is one statement, and splitting it invites being heard as two
          unrelated ones. */}
      <span className="hidden md:inline"> {detail}</span>
    </aside>
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
    <div className="flex shrink-0 items-center gap-2 border-b border-line bg-sunken px-4 py-1">
      <label htmlFor="find-in-session" className="text-3xs font-medium text-ink-muted">
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
        className="flex-1 rounded border border-line bg-raised px-2 py-0.5 text-xs focus-ring"
      />
      <button
        type="button"
        onClick={onClose}
        className="rounded px-1.5 text-2xs text-ink-muted hover:bg-active hover:text-ink focus-ring"
      >
        Close
      </button>
    </div>
  );
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
  fidelity,
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
  /**
   * The session's, threaded to every `Timeline` below.
   *
   * A property of the *record*, so it is the same for every turn in the session — which
   * is why it is read once here rather than per turn. It gates whether a delegation
   * duration may be drawn at all; see `lib/timeline-waterfall.ts`.
   */
  fidelity: Fidelity;
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
      <output aria-live="polite" className="block py-8 text-center text-sm text-ink-muted">
        Loading conversation...
      </output>
    );
  }

  if (error) {
    return (
      <div role="alert" className="rounded-lg border border-danger-line bg-danger-bg p-4">
        <p className="text-sm font-medium text-danger">
          Could not load the conversation history.
        </p>
        <p className="mt-1 text-sm text-danger">
          {error instanceof Error ? error.message : "Unknown error."}
        </p>
        <button
          type="button"
          onClick={onRetry}
          className="mt-3 rounded border border-danger-line bg-raised px-3 py-1.5 text-sm font-medium hover:bg-danger-bg focus-ring"
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
          <p className="text-sm font-medium text-ink-soft">No turns yet</p>
          <p className="mt-1 text-sm text-ink-muted">
            Send a message below to start the conversation.
          </p>
        </div>
      )}
      {empty && filtering && (
        <p className="py-12 text-center text-sm text-ink-muted">No turns match that text.</p>
      )}

      {past.map((entry) => (
        <TurnSection
          key={entry.turn.id}
          entry={entry}
          fidelity={fidelity}
          onResend={onResend}
          disabled={isLive}
          onOpenFile={onOpenFile}
          expandAll={expandAll}
        />
      ))}

      {liveTurnId && !filtering && (
        <LiveTurn
          turnId={liveTurnId}
          // A live turn is always `stream` in fact — it *is* the process's own stdio,
          // which is the only thing that produces that fidelity — but the session's
          // value is passed rather than hardcoded here. Two reasons: a hardcoded
          // `"stream"` is a claim this component is not the authority on, and a watched
          // session streams events from a *file* the vendor is flushing, where the
          // record genuinely is coarse while the turn is genuinely live.
          fidelity={fidelity}
          isLive={isLive}
          onOpenFile={onOpenFile}
          expandAll={expandAll}
        />
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
  fidelity,
  onResend,
  disabled,
  onOpenFile,
  expandAll,
}: {
  entry: TurnWithEvents;
  fidelity: Fidelity;
  onResend: (text: string) => void;
  disabled: boolean;
  onOpenFile: (path: string, line?: number) => void;
  expandAll: boolean | null;
}) {
  const [copied, setCopied] = useState(false);

  // Resending a turn that already succeeded would duplicate work the agent has
  // done, so it is offered only where the turn produced no usable result.
  const t = useT();
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
    <section aria-label={`Turn ${entry.turn.id}`} className="border-t border-line pt-1 first:border-t-0">
      {/* A turn nobody asked for has to say so. Its `requestText` is empty, so
          unlabelled it renders as an answer to a message the user never wrote —
          and the thing it is usually reporting is work they had stopped expecting
          to hear about, which makes the provenance the most useful part. */}
      {entry.turn.origin === "agent" && (
        <p className="flex items-center gap-1.5 pt-1 text-3xs text-ink-muted">
          <span className="rounded bg-hover px-1">unprompted</span>
          <span>the agent reported this on its own</span>
        </p>
      )}

      <Timeline
        events={entry.events}
        fidelity={fidelity}
        onOpenFile={onOpenFile}
        expandAll={expandAll}
      />

      {entry.turn.errorMessage && (
        <p role="alert" className="mt-1 text-xs text-danger">
          {entry.turn.errorMessage}
        </p>
      )}

      <div className="flex items-center gap-2 py-1 text-3xs text-ink-muted">
        <TurnStatusBadge status={entry.turn.status} />
        {duration !== undefined && <span>{duration.toFixed(1)}s</span>}
        <button
          type="button"
          onClick={() => void copy()}
          className="rounded px-1 hover:bg-hover hover:text-ink-soft focus-ring"
        >
          {copied ? t("action.copied") : t("action.copy")}
        </button>
        {resendable && entry.turn.requestText && (
          <button
            type="button"
            onClick={() => onResend(entry.turn.requestText)}
            disabled={disabled}
            className="rounded px-1 hover:bg-hover hover:text-ink-soft focus-ring disabled:opacity-50"
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
  fidelity,
  isLive,
  onOpenFile,
  expandAll,
}: {
  turnId: string;
  fidelity: Fidelity;
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
    const scroller = node.closest("[data-scroller]");
    if (!scroller) return;
    const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    pinnedRef.current = distance < 120;
    if (pinnedRef.current) node.scrollIntoView({ block: "end" });
  }, [events]);

  return (
    <section aria-label="Current turn" className="border-t border-line pt-1">
      <Timeline
        events={events}
        fidelity={fidelity}
        onOpenFile={onOpenFile}
        expandAll={expandAll}
      />

      <div className="flex items-center gap-1.5 py-1 text-3xs">
        <span
          aria-hidden="true"
          className={`h-1.5 w-1.5 rounded-full ${
            status === "open"
              ? "animate-pulse bg-info-solid"
              : status === "reconnecting"
                ? "bg-warning-solid"
                : "bg-line-strong"
          }`}
        />
        <span aria-live="polite" className="text-ink-muted">
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
