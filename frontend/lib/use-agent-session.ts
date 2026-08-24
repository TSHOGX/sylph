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
import { api, type LinkState, type PendingRequest } from "@/lib/api";

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
  /** True while the first handshake is in flight — it can take seconds. */
  connecting: boolean;
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
  steer: (text: string) => void;
}

export function useAgentSession(sessionId: string | undefined): AgentSession {
  const queryClient = useQueryClient();
  const enabled = Boolean(sessionId);

  // Establishing a link runs an agent CLI, so this is slow once and cached after.
  // `staleTime: Infinity` with explicit invalidation on a `capabilities` event is
  // deliberate: refetching on window focus would spawn an agent process every time
  // the user tabbed back.
  const linkQuery = useQuery({
    queryKey: ["session", sessionId, "capabilities"],
    queryFn: () => api.sessions.capabilities(sessionId!),
    enabled,
    staleTime: Infinity,
    retry: false,
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
    mutationFn: (text: string) => api.sessions.steer(sessionId!, text),
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
    connecting: linkQuery.isLoading,
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
    steer: (text: string) => steerMutation.mutate(text),
  };
}
