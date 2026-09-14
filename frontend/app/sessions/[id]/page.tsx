"use client";

/**
 * A session: the agent-workspace conversation canvas.
 *
 * The transcript is one continuous timeline rather than a list of per-turn cards.
 * A turn boundary is a rule with a status line, not a container — an agent's work
 * reads as a single log, and boxing each turn re-introduces chat framing
 * (`docs/ui-layout.md` § Conversation canvas).
 *
 * The server's execution projection supplies active and archived content through
 * one bounded read path. Turn metadata supplies control state separately.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import dynamic from "next/dynamic";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  ApiError,
  type Facet,
  type ControlBlock,
  type UnavailableReason,
} from "@/lib/api";
import { useSessionIdParam } from "@/lib/use-session-id-param";
import { useT } from "@/lib/i18n";
import type { MessageKey, Translate } from "@/lib/i18n/locale";
import { ErrorBoundary } from "@/lib/error-boundary";
import { formatTimestamp, sessionLabel } from "@/lib/format";
import { densityClasses, useSettings } from "@/lib/settings";
import { relayName, useEnvironments } from "@/lib/environments";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { TERMINAL_TURN_STATUSES as TERMINAL } from "@/lib/turn-status";
import { useAgentSession } from "@/lib/use-agent-session";
import { refreshRereadsTheStore } from "@/lib/refresh-transcript";
import { createSubmissionIdentity } from "@/lib/submission-identity";
import { ApprovalDialog } from "@/components/approval-dialog";
import { type StagedAttachment } from "@/components/attachment-picker";
import { Composer } from "@/components/composer";
import { ExecutionTimeline } from "@/components/execution-timeline";
import {
  EnvironmentChip,
  FidelityMarker,
  SessionStatusBadge,
} from "@/components/badges";
import { Button, IconButton } from "@/components/ui/button";
import { MenuButton } from "@/components/ui/menu";
import { CopyIcon, MoreIcon, RefreshIcon } from "@/components/icons";

const ControlQueue = dynamic(() =>
  import("@/components/control-queue").then((module) => module.ControlQueue),
);

export default function SessionPage() {
  const t = useT();
  // Decoded by the hook, never read raw: the App Router hands back the raw path
  // segment, and a session id is the identity triple, so it is percent-encoded there.
  // `use-session-id-param` holds the reasoning and the three ways this failed.
  const sessionId = useSessionIdParam() ?? "";
  const queryClient = useQueryClient();
  const { settings } = useSettings();
  const {
    showArtifact,
    registerSessionActions,
    setActiveWorkdir,
    rereadSession,
    rereadNotice,
  } = useWorkspaceUI();
  const scroller = useRef<HTMLDivElement>(null);

  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<StagedAttachment[]>([]);
  const [submissionIdentity] = useState(createSubmissionIdentity);
  const [sentTurn, setSentTurn] = useState<{
    id: string;
    sessionId: string;
  } | null>(null);
  const [collapseEpoch, setCollapseEpoch] = useState(0);
  const [find, setFind] = useState<string | null>(null);
  const hasCommittedWork = useRef(false);

  const sessionQuery = useQuery({
    queryKey: ["session", sessionId],
    queryFn: ({ signal }) => api.sessions.get(sessionId, signal),
    refetchInterval: 1000,
  });

  const claim = sentTurn?.sessionId === sessionId ? sentTurn : null;
  const historyQuery = useQuery({
    queryKey: ["session", sessionId, "turn-state", claim?.id],
    gcTime: 0,
    queryFn: ({ signal }) => api.sessions.turnStates(sessionId, signal),
    refetchInterval: 1000,
  });
  const session = sessionQuery.data?.session;
  const history = useMemo(
    () => historyQuery.data?.turns ?? [],
    [historyQuery.data],
  );

  // A new accepted turn changes the metadata query key. A request made before
  // Send therefore cannot retire the claim by delivering a stale snapshot late.
  // Once that fresh query succeeds, absence also means settled: this window
  // includes the active turn even after older terminal turns leave it.
  const sentState = history.find((entry) => entry.turn.id === claim?.id)?.turn;
  const sentSettled =
    claim &&
    Boolean(historyQuery.data) &&
    (!sentState || TERMINAL.has(sentState.status));
  const liveTurnId =
    (!sentSettled ? claim?.id : null) ??
    history.find((entry) => !TERMINAL.has(entry.turn.status))?.turn.id ??
    null;
  const isLive = Boolean(liveTurnId);

  const sendTurn = useMutation({
    // `override` carries a resent turn's text. Its attachments are deliberately
    // not resent: they were staged from local files this tab no longer holds.
    mutationFn: async (override?: string) => {
      const draft = text;
      const staged = attachments;
      const payload = {
        text: (override ?? draft).trim(),
        attachments: override
          ? []
          : staged.map(({ name, type, data }) => ({ name, type, data })),
      };
      const idempotencyKey = submissionIdentity.key({
        sessionId,
        kind: "send",
        ...payload,
      });
      const result = await api.turns.create(sessionId, {
        ...payload,
        idempotencyKey,
      });
      submissionIdentity.accepted(idempotencyKey);
      if (override === undefined) {
        setText((current) => (current === draft ? "" : current));
        setAttachments((current) => (current === staged ? [] : current));
      }
      return result;
    },
    onSuccess: (result) => {
      hasCommittedWork.current = true;
      setSentTurn({ id: result.turn.id, sessionId });
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
      void queryClient.invalidateQueries({
        queryKey: ["session", sessionId, "turn-state"],
      });
    },
  });

  const sendControl = useMutation({
    mutationFn: async ({
      kind,
      value,
      turnId,
    }: {
      kind: "steer" | "queue" | "native_steer" | "native_queue";
      value: string;
      turnId?: string | null;
    }) => {
      const payload = {
        sessionId,
        kind,
        text: value,
        ...(kind === "steer" ? { turnId } : {}),
      };
      const key = submissionIdentity.key(payload);
      const result =
        kind === "steer"
          ? await api.sessions.steer(sessionId, value, turnId!, key)
          : kind === "queue"
            ? await api.sessions.queue(sessionId, value, key)
            : await api.sessions.nativeQueue(
                sessionId,
                value,
                kind === "native_steer",
                key,
              );
      submissionIdentity.accepted(key);
      setText((current) => (current === value ? "" : current));
      return result;
    },
    onSettled: () => {
      void queryClient.invalidateQueries({
        queryKey: ["session", sessionId, "controls"],
      });
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
    mutationFn: async ({
      command,
      args,
    }: {
      command: string;
      args: Record<string, string>;
    }) => {
      const draft = text;
      const idempotencyKey = submissionIdentity.key({
        sessionId,
        command,
        args,
      });
      const result = await api.turns.create(sessionId, {
        command,
        args,
        idempotencyKey,
      });
      submissionIdentity.accepted(idempotencyKey);
      setText((current) => (current === draft ? "" : current));
      return result;
    },
    onSuccess: (result) => {
      hasCommittedWork.current = true;
      setSentTurn({ id: result.turn.id, sessionId });
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
      void queryClient.invalidateQueries({
        queryKey: ["session", sessionId, "turn-state"],
      });
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
  const [selectedRequest, setSelectedRequest] = useState<string | null>(null);
  const currentRequest =
    agent.requests.find((request) => request.id === selectedRequest) ??
    agent.current;
  const pendingRequest =
    currentRequest && currentRequest.id !== dismissedRequest
      ? currentRequest
      : undefined;

  const mutateTurn=sendTurn.mutate;
  const resend=useCallback((value:string)=>mutateTurn(value),[mutateTurn]);
  const reviewRequest = useCallback((id: string) => {
    setSelectedRequest(id);
    setDismissedRequest(null);
  }, []);
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
          case "collapseAll":
            setCollapseEpoch((value) => value + 1);
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
      <div
        role="alert"
        className="flex flex-1 items-center justify-center px-4"
      >
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
              name={
                environments.nameOf(session.environment) ?? session.environment
              }
              reachableVia={relayName(session.environment, environments)}
            />
          )}
          {session.status === "running" && (
            <SessionStatusBadge status={session.status} />
          )}
          <div className="hidden shrink-0 items-center gap-1 md:flex">
            <Button
              variant="ghost"
              onClick={() => setCollapseEpoch((value) => value + 1)}
            >
              {t("session.collapseAll")}
            </Button>
          </div>
          {/* The same two actions plus the ones that had no home, folded into one
              button on a phone. `Cmd+F` and `Shift+C` reach three of these
              on a desktop, and a phone has no modifier keys at all — so without this
              they were unreachable rather than merely inconvenient. */}
          <div className="shrink-0 md:hidden">
            <MenuButton
              label={t("session.actions")}
              align="end"
              items={[
                {
                  label: t("session.collapseAllBlocks"),
                  onSelect: () => setCollapseEpoch((value) => value + 1),
                },
                {
                  label: t("session.filterTurnsEllipsis"),
                  separated: true,
                  onSelect: () =>
                    setFind((current) => (current === null ? "" : current)),
                },
                {
                  label: t("session.copyWorkspacePath"),
                  icon: <CopyIcon size={14} />,
                  onSelect: () =>
                    void navigator.clipboard
                      .writeText(session.workdir)
                      .catch(() => {}),
                },
                ...(refreshRereadsTheStore(session.fidelity)
                  ? [
                      {
                        label: t("session.refreshTranscript"),
                        icon: <RefreshIcon size={14} />,
                        disabled:
                          rereadNotice?.state === "pending" &&
                          rereadNotice.sessionId === sessionId,
                        onSelect: () =>
                          rereadSession(sessionId, sessionLabel(session)),
                      },
                    ]
                  : []),
              ]}
            >
              {(trigger) => (
                <IconButton label={t("session.actions")} size={20} {...trigger}>
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
        <FindBar
          value={find}
          onChange={setFind}
          onClose={() => setFind(null)}
          t={t}
        />
      )}

      {/* What Sylph can and cannot do with this session right now, when that is not
          "everything". This replaces the native-session policy banner, which promised
          Sylph would not reset or compact a session — a promise kept by construction,
          since nothing ever reset anything, so it described an absence as a feature on
          every one of those sessions forever. This says something that can actually
          change and that the user can sometimes act on. */}
      <DriveState
        control={agent.control}
        controlBlock={agent.controlBlock}
        connecting={agent.connecting}
        error={agent.linkError}
        onRetry={agent.retryLinkRead}
        t={t}
      />

      {/*
        `data-scroller` rather than being found by its utility class. `LiveTurn` needs
        this node to decide whether the user is pinned to the bottom, and it used to
        locate it with `closest(".overflow-y-auto")` — which silently breaks the
        moment a scroll container is expressed differently, and did break when this
        became the `pane` utility. An explicit contract cannot drift.
      */}
      <div
        ref={scroller}
        data-scroller
        tabIndex={0}
        aria-label="Conversation"
        className="pane min-h-0 flex-1 px-3 py-3 md:px-4"
      >
        <div className={`mx-auto max-w-3xl ${density.text}`}>
          <ErrorBoundary label="the transcript">
            <ExecutionTimeline
              sessionId={sessionId}
              scroller={scroller}
              fidelity={session.fidelity}
              collapseEpoch={collapseEpoch}
              onOpenFile={openFile}
              find={find}
              turns={history}
              onResend={resend}
              isLive={isLive}
              onReviewRequest={reviewRequest}
            />
          </ErrorBoundary>
        </div>
      </div>

      {/* A lost race means the panel on screen was already answered — by another
          frontend, or another tab. Worth saying: the user pressed a button and
          nothing happened, and the reason is not their mistake. `staleAnswer` was
          computed for this and had no surface. */}
      {agent.staleAnswer && (
        <p
          role="status"
          className="border-t border-line bg-canvas px-3 pt-2 text-2xs text-ink-muted"
        >
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

      <ControlQueue
        sessionId={sessionId}
        canSteer={agent.can("steer")}
        canQueue={agent.can("queue")}
        turnId={liveTurnId}
      />
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
        sending={
          sendTurn.isPending || sendControl.isPending || runCommand.isPending
        }
        sendError={sendControl.error ?? sendTurn.error ?? runCommand.error}
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
        capabilityError={agent.linkError}
        control={agent.control}
        controlBlock={agent.controlBlock}
        onReattach={agent.linkError ? agent.retryLinkRead : agent.reattach}
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
        // already told us we cannot keep. Native queues declare their own semantics; a minimal adapter
        // may support neither.
        canInterrupt={agent.can("interrupt")}
        onInterrupt={() => agent.interrupt()}
        canSteer={agent.can("steer")}
        canQueue={agent.can("queue")}
        // The running turn is named as a precondition, so a steer that races the
        // turn boundary is refused rather than delivered into the next conversation.
        canQueueSteering={agent.can("queue_steering")}
        onNativeQueue={(value, priority) =>
          sendControl.mutate({
            kind: priority ? "native_steer" : "native_queue",
            value,
          })
        }
        onSteer={(value) =>
          sendControl.mutate({ kind: "steer", value, turnId: liveTurnId })
        }
        // No turn id: a queued follow-up is *for* after the turn, so a boundary crossing
        // underneath it changes nothing and there is no precondition to state.
        onQueue={(value) => sendControl.mutate({ kind: "queue", value })}
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
function DriveState({
  control,
  controlBlock,
  connecting,
  error,
  onRetry,
  t,
}: {
  control?: Facet;
  controlBlock?: ControlBlock;
  connecting: boolean;
  error: unknown;
  onRetry: () => void;
  t: Translate;
}) {
  if (connecting) return null;

  if (error) {
    return (
      <aside
        role="alert"
        className="shrink-0 border-b border-line bg-sunken px-3 py-2 text-xs"
      >
        <p className="font-medium text-danger">{t("drive.readFailed")}</p>
        <p className="text-ink-muted">
          {error instanceof Error ? error.message : String(error)}
        </p>
        <Button variant="ghost" onClick={onRetry}>
          {t("app.retry")}
        </Button>
      </aside>
    );
  }

  if (controlBlock?.kind === "unauthorized") {
    return (
      <aside
        role="status"
        aria-label={t("drive.label")}
        className="shrink-0 border-b border-line bg-sunken px-3 py-1.5 text-2xs text-ink-soft md:px-4"
      >
        {controlBlock.reason === "expired"
          ? t("drive.unauthorized.expired")
          : t("drive.unauthorized.notGranted")}
      </aside>
    );
  }

  const blockedReason =
    controlBlock?.kind === "unavailable"
      ? controlBlock.reason
      : control?.available
        ? undefined
        : control?.reason;
  if (!blockedReason) return null;

  // Exhaustive over the closed union, so a reason added to the protocol is a compile
  // error here rather than a session silently rendering no explanation. The map holds
  // catalogue key *stems* rather than sentences: the pair is always `.headline` and
  // `.detail`, so one entry per reason cannot drift into naming two unrelated keys.
  const stem: Record<UnavailableReason, string> = {
    environment_unreachable: "drive.environmentUnreachable",
    not_ours_to_drive: "drive.notOursToDrive",
    no_agent: "drive.noAgent",
    handshake_failed: "drive.handshakeFailed",
  };
  const headline = t(`${stem[blockedReason]}.headline` as MessageKey);
  const detail = t(`${stem[blockedReason]}.detail` as MessageKey);

  return (
    <aside
      role="status"
      aria-label={t("drive.label")}
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
  t,
}: {
  value: string;
  onChange: (value: string) => void;
  onClose: () => void;
  t: Translate;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => ref.current?.focus(), []);

  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-line bg-sunken px-4 py-1">
      <label
        htmlFor="find-in-session"
        className="text-3xs font-medium text-ink-muted"
      >
        {t("find.label")}
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
