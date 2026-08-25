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
  type LinkFailure,
  type LinkState,
  type PendingRequest,
} from "@/lib/api";

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
  /** Whether an agent process is attached right now. */
  attached: boolean;
  /**
   * True while a handshake is in flight — it spawns an agent CLI and can take
   * seconds. Covers a retry as well as the first attempt, because to the user
   * they are the same wait and both need the same "not broken, just slow" signal.
   */
  connecting: boolean;
  /** Why there is no link, when an attempt failed. */
  failure?: LinkFailure;
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

export function useAgentSession(sessionId: string | undefined): AgentSession {
  const queryClient = useQueryClient();
  const enabled = Boolean(sessionId);

  // Establishing a link runs an agent CLI, so this is slow once and cached after.
  // `staleTime: Infinity` with explicit invalidation on a `capabilities` event is
  // deliberate: refetching on window focus would spawn an agent process every time
  // the user tabbed back.
  //
  // The stale time is conditional because a failure is *also* a 200 here — the
  // endpoint reports detached-with-reason rather than erroring, so the session
  // stays readable. Cached forever, that made one failed handshake permanent for
  // the life of the page: `/` stayed dead until a turn was sent or the tab
  // reloaded, which is what made the symptom look intermittent rather than
  // configured. A recoverable failure is therefore allowed to go stale, so the
  // next mount or explicit retry tries again.
  //
  // `no_adapter` is exempt: no number of retries installs an adapter, and each
  // attempt costs a process spawn. That one is cached like a success and cleared
  // only by the user retrying or by a `link` event saying otherwise.
  const linkQuery = useQuery({
    queryKey: ["session", sessionId, "capabilities"],
    queryFn: () => api.sessions.capabilities(sessionId!),
    enabled,
    staleTime: (query) => {
      const failure = query.state.data?.failure;
      if (!failure) return Infinity;
      return failure.code === "no_adapter" ? Infinity : 0;
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
          `/api/v1/sessions/${sessionId}/events`,
          { signal: controller.signal, headers: { Accept: "text/event-stream" } },
        );
        if (!response.ok || !response.body) return;

        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let carry = "";

        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          carry += decoder.decode(value, { stream: true });

          // Frames are separated by a blank line. Only the event name matters
          // here — the payload is a hint that something changed, and the queries
          // above are what actually hold the state.
          const frames = carry.split("\n\n");
          carry = frames.pop() ?? "";
          for (const frame of frames) {
            if (frame.includes("event: request")) {
              void queryClient.invalidateQueries({
                queryKey: ["session", sessionId, "requests"],
              });
            }
            if (frame.includes("event: capabilities") || frame.includes("event: link")) {
              void queryClient.invalidateQueries({
                queryKey: ["session", sessionId, "capabilities"],
              });
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
    attached: Boolean(link?.attached),
    // `isFetching` rather than `isLoading`, so a retry reads as connecting too: a
    // refetch over cached failure data is not "loading" to React Query, but it is
    // exactly the wait the user needs told about.
    connecting: linkQuery.isFetching || reattachMutation.isPending,
    failure: link?.failure,
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
    setConfig: (configId: string, value: string | boolean) =>
      configMutation.mutate({ configId, value }),
    settingConfig: configMutation.isPending,
    configError: configMutation.error,
  };
}
