"use client";

/**
 * A session's live agent: what it can do, and what it is blocked on.
 *
 * Two facts that are neither turn-scoped nor static, which is why they need their
 * own hook rather than living in the page's turn queries:
 *
 * * **Capabilities** come from a live handshake and change while a session runs —
 *   a skill edited on disk, a plugin loaded. A composer that cached them at mount
 *   would offer a stale command list.
 * * **Pending requests** block the agent, so they outlive any client. One raised
 *   while this browser was closed must appear on reconnect, which means reading a
 *   resource rather than only watching a stream.
 *
 * The session-scoped SSE stream is the fast path and the query is the truth. Both,
 * because a stream-only design strands a request when a client disconnects, and a
 * poll-only design shows an approval seconds late while the agent waits.
 */

import { useCallback, useEffect } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  isRetryable,
  segment,
  type Facet,
  type LinkState,
  type PendingRequest,
} from "@/lib/api";
import { createSSEDecoder } from "@/lib/sse-parse";
import { mergeRequestRow, parseRequestFrame } from "@/lib/pending-requests";

/**
 * How often to re-read pending requests as a backstop.
 *
 * The stream normally delivers a request within milliseconds, so this only covers
 * a dropped connection. Deliberately slow: an agent blocked for twenty extra
 * seconds is bad, but polling a resource nobody is waiting on is worse for a tool
 * that runs all day.
 */
const REQUEST_POLL_MS = 20_000;

export interface AgentSession {
  /** Live capability surface, or undefined until the first handshake lands. */
  link?: LinkState;
  /**
   * Can Sylph see what this session is doing, and why not.
   *
   * Separate from {@link control} rather than flattened into one `attached`, because
   * the two genuinely differ and the interesting case is where they disagree: a
   * session running in a terminal is observable and undrivable, so a surface that
   * read one boolean would either hide readable history or offer a composer that
   * cannot send. Defaults to unavailable-for-an-unreachable-environment before the
   * first handshake, which is the honest answer while nothing has been asked.
   */
  observe: Facet;
  /** Can Sylph send this session a turn, and why not. */
  control: Facet;
  /**
   * True while a handshake is in flight — it spawns an agent CLI and can take
   * seconds. Covers a retry as well as the first attempt, because to the user
   * they are the same wait and both need the same "not broken, just slow" signal.
   */
  connecting: boolean;
  /** Establish a link now, after a failure. */
  reattach: () => void;
  requests: PendingRequest[];
  /** The oldest unanswered request: what a dialog should show. */
  current?: PendingRequest;
  /** Whether the agent declared it can do something. */
  can: (capability: string) => boolean;
  resolve: (option: string, input?: unknown) => void;
  resolving: boolean;
  /** Set when the last answer lost a race, so the UI can say the dialog was stale. */
  staleAnswer: boolean;
  interrupt: () => void;
  steer: (text: string, turnId?: string) => void;
  /** Add text to run after the turn in flight. */
  queue: (text: string) => void;
  /** Set one typed config option; the response replaces the cached surface. */
  setConfig: (configId: string, value: string | boolean) => void;
  settingConfig: boolean;
  /**
   * Why the last config change failed, when one did.
   *
   * Surfaced rather than swallowed because a refused set is otherwise
   * indistinguishable from a broken control: the value simply does not change, with
   * nothing said.
   */
  configError: unknown;
}

/**
 * What a facet reads as before any handshake has answered.
 *
 * `environment_unreachable` rather than a bare `available: false`, because an
 * unexplained false is the original defect this whole vocabulary exists to fix. It
 * is also the accurate reading: nothing has reached that machine yet. `connecting`
 * is what distinguishes "on its way" from "settled", and it outranks this reason at
 * every surface that renders one.
 */
const UNREACHED: Facet = { available: false, reason: "environment_unreachable" };

export function useAgentSession(sessionId: string | undefined): AgentSession {
  const queryClient = useQueryClient();
  const enabled = Boolean(sessionId);

  // Establishing a link runs an agent CLI, so this is slow once and cached after.
  // `staleTime: Infinity` with explicit invalidation on a `capabilities` event is
  // deliberate: refetching on window focus would spawn an agent process every time
  // the user tabbed back.
  //
  // The stale time is conditional because a failure is *also* a 200 here — the
  // endpoint reports unavailable-with-a-reason rather than erroring, so the session
  // stays readable. Cached forever, that made one failed handshake permanent for
  // the life of the page: `/` stayed dead until a turn was sent or the tab
  // reloaded, which is what made the symptom look intermittent rather than
  // configured.
  //
  // Which reasons may go stale is `isRetryable`'s answer, not a condition spelled
  // out here — it mirrors `UnavailableReason::is_retryable` in
  // `crates/protocol/src/link.rs`, so the rule has one definition rather than a copy
  // in every client that caches. `no_agent` and `not_ours_to_drive` cache like a
  // success because no retry installs a CLI or teaches a vendor a new channel;
  // `environment_unreachable` must not, because a laptop wakes up.
  //
  // Keyed on `control`, which is the facet a retry is for: establishing a link is
  // what `POST /link` does, and observation answers from the server's store whether
  // or not that succeeds. An unreachable environment therefore goes stale on the
  // strength of the control facet even while observe is fine.
  const linkQuery = useQuery({
    queryKey: ["session", sessionId, "capabilities"],
    queryFn: () => api.sessions.capabilities(sessionId!),
    enabled,
    staleTime: (query) => {
      const control = query.state.data?.link.control;
      if (!control || control.available) return Infinity;
      // An unavailable facet always carries a reason (`Facet`'s constructors enforce
      // it server-side). A missing one is a server that broke that invariant, and
      // treating it as retryable is the safe reading: a wasted refetch, rather than
      // a failure pinned for the life of the page.
      return control.reason && !isRetryable(control.reason) ? Infinity : 0;
    },
    retry: false,
  });

  // Re-establish on demand. The endpoint is `POST /link`, which establishes
  // eagerly, and its response *is* the new surface — so it is written straight
  // into the cache rather than triggering a second round trip.
  const reattachMutation = useMutation({
    mutationFn: () => api.sessions.attach(sessionId!),
    onSuccess: (link) => {
      queryClient.setQueryData(["session", sessionId, "capabilities"], link);
    },
    onError: () => {
      // A failed attach returns an error status rather than a body, so there is
      // nothing to write. Invalidating lets the GET report the classified reason.
      void queryClient.invalidateQueries({
        queryKey: ["session", sessionId, "capabilities"],
      });
    },
  });

  const requestsQuery = useQuery({
    queryKey: ["session", sessionId, "requests"],
    queryFn: () => api.sessions.requests(sessionId!),
    enabled,
    refetchInterval: REQUEST_POLL_MS,
  });

  // The session stream carries facts that are not about a turn: a link attaching
  // or dying, a capability change, a request raised. Subscribed here rather than
  // in the page because this hook is what owns the state those facts invalidate.
  useEffect(() => {
    if (!sessionId) return;

    const controller = new AbortController();

    async function listen() {
      try {
        const response = await fetch(
          // `segment`, not raw interpolation: a session id is the identity triple, so it
          // holds `:` — and a native id can hold `/` besides, since `claude` names a
          // delegated session `<parent>/agent-a<name>-<hash>`. Raw, the browser encodes
          // the colons itself and the backend refuses the result, because
          // `SessionId::decode` splits on a literal `:` rather than decoding first.
          // `sessionId!` for the same reason the queries above use it: the effect returns
          // early when it is absent, but that guard does not narrow inside this nested
          // function.
          `/api/v1/sessions/${segment(sessionId!)}/events`,
          { signal: controller.signal, headers: { Accept: "text/event-stream" } },
        );
        if (!response.ok || !response.body) return;

        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        // The same parser the turn stream uses, rather than `split("\n\n")` and
        // `frame.includes("event: request")`. Two reasons, and the second is why the
        // payload can now be trusted at all: a substring test matches the *body* as
        // readily as the name, so any request whose title quoted the text `event:
        // request` triggered its own branch — and matching `"event: request"` as a
        // prefix cannot distinguish a kind added later that merely starts the same way.
        // Reading the name as a field settles both, and having a parsed frame is what
        // makes `data` available as a value instead of as text inside a haystack.
        const frames = createSSEDecoder();

        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;

          for (const frame of frames.push(decoder.decode(value, { stream: true }))) {
            if (frame.event === "request") {
              // Applied, not invalidated. The frame carries the whole row, so the set
              // this client is blocked on is recomputed from it with **no round trip** —
              // which is the point: an answer given on a phone should retire the panel on
              // a laptop as fast as the frame arrives, and a refetch adds a request the
              // server has already answered.
              //
              // The invalidate is the fallback for a frame this client could not parse,
              // so it is in the `else`. Running it unconditionally performs exactly the
              // round trip `setQueryData` exists to avoid, which reads as a belt-and-braces
              // reconciler and is really the cost the optimisation was meant to remove.
              const row = parseRequestFrame(frame.data);
              if (row) {
                queryClient.setQueryData<PendingRequest[]>(
                  ["session", sessionId, "requests"],
                  (current) => mergeRequestRow(current, row),
                );
              } else {
                void queryClient.invalidateQueries({
                  queryKey: ["session", sessionId, "requests"],
                });
              }
            }
            if (frame.event === "capabilities" || frame.event === "link") {
              void queryClient.invalidateQueries({
                queryKey: ["session", sessionId, "capabilities"],
              });
            }
            // The agent started a turn nobody asked for — reporting backgrounded work
            // that finished after the turn which dispatched it. This is the only
            // notice a client gets: the turn's own events go to its turn-scoped
            // stream, which nothing is attached to yet, and refetching history is what
            // makes the page adopt it and start streaming. Without this the turn is
            // real and journaled but invisible until a reload, which reads to the user
            // as the notification never having arrived.
            if (frame.event === "turn-opened") {
              void queryClient.invalidateQueries({
                queryKey: ["session", sessionId, "history"],
              });
              void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
            }
          }
        }
      } catch {
        // An aborted fetch on unmount, or a dropped connection. The poll above is
        // the backstop, so a reconnect loop here would add nothing but load.
      }
    }

    void listen();
    return () => controller.abort();
  }, [sessionId, queryClient]);

  const resolveMutation = useMutation({
    mutationFn: ({ id, option, input }: { id: string; option: string; input?: unknown }) =>
      api.requests.resolve(id, option, input),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "requests"] });
    },
  });

  const interruptMutation = useMutation({
    mutationFn: () => api.sessions.interrupt(sessionId!),
  });

  const steerMutation = useMutation({
    mutationFn: ({ text, turnId }: { text: string; turnId?: string }) =>
      api.sessions.steer(sessionId!, text, turnId),
  });

  // No turn id, and that is the difference rather than an omission: a queued follow-up is
  // *for* after the turn, so there is no precondition a boundary could invalidate.
  const queueMutation = useMutation({
    mutationFn: (text: string) => api.sessions.queue(sessionId!, text),
  });

  // The response *is* the new surface, so it is written into the cache rather than
  // triggering a refetch. Setting one option can change another's choices — effort
  // levels depend on the selected model for both installed agents — so an optimistic
  // local value would be wrong exactly when it matters.
  //
  // The failure path is not decoration. An agent can *refuse* a set — an adapter
  // sending the wrong field name to its vendor CLI gets a failed RPC back — and with
  // no `onError` the rejection had nowhere to go: the surface kept its old value, the
  // control re-rendered from it, and the only observable outcome was that clicking a
  // model did nothing. That is precisely how a real adapter bug presented as a broken
  // picker. Invalidating re-reads the true surface rather than leaving a value this
  // client only believes it set, and `configError` is what lets the UI say so.
  const configMutation = useMutation({
    mutationFn: ({ configId, value }: { configId: string; value: string | boolean }) =>
      api.sessions.setConfig(sessionId!, configId, value),
    onSuccess: (link) => {
      queryClient.setQueryData(["session", sessionId, "capabilities"], link);
      // Launch-equivalent settings are persisted by the backend. Refresh the
      // session projection as well so any consumer deriving labels or flags from
      // `flagsJson` observes the same state immediately, rather than only after a
      // full reload or a detach/reattach.
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
    },
    onError: () => {
      void queryClient.invalidateQueries({
        queryKey: ["session", sessionId, "capabilities"],
      });
    },
  });

  const requests = requestsQuery.data ?? [];
  // Oldest first, because that is the one the agent is actually blocked on. The
  // backend already orders them this way; relying on it rather than re-sorting
  // keeps one definition of "next".
  const current = requests[0];

  const link = linkQuery.data;
  const can = useCallback(
    (capability: string) => Boolean(link?.supports?.includes(capability)),
    [link],
  );

  const resolve = useCallback(
    (option: string, input?: unknown) => {
      if (!current) return;
      resolveMutation.mutate({ id: current.id, option, input });
    },
    [current, resolveMutation],
  );

  return {
    link,
    // Before the first handshake both facets read as unreached rather than
    // available. Defaulting the other way would render a composer that can send to
    // a session nothing has reached yet.
    observe: link?.link.observe ?? UNREACHED,
    control: link?.link.control ?? UNREACHED,
    // `isFetching` rather than `isLoading`, so a retry reads as connecting too: a
    // refetch over cached failure data is not "loading" to React Query, but it is
    // exactly the wait the user needs told about.
    connecting: linkQuery.isFetching || reattachMutation.isPending,
    reattach: () => reattachMutation.mutate(),
    requests,
    current,
    can,
    resolve,
    resolving: resolveMutation.isPending,
    // A lost race means the dialog on screen was stale, which is worth telling the
    // user rather than silently swallowing.
    staleAnswer:
      resolveMutation.isError &&
      /already/i.test(String((resolveMutation.error as Error | null)?.message ?? "")),
    interrupt: () => interruptMutation.mutate(),
    steer: (text: string, turnId?: string) => steerMutation.mutate({ text, turnId }),
    queue: (text: string) => queueMutation.mutate(text),
    setConfig: (configId: string, value: string | boolean) =>
      configMutation.mutate({ configId, value }),
    settingConfig: configMutation.isPending,
    configError: configMutation.error,
  };
}
