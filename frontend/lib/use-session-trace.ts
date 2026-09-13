"use client";

import { useEffect, useRef, useState } from "react";
import type { StreamEvent } from "@/lib/api-types";
import { segment } from "@/lib/api";
import { createSSEDecoder } from "@/lib/sse-parse";

export type SessionTraceStatus = "connecting" | "open" | "reconnecting" | "closed";

interface SessionTraceState {
  sessionId?: string;
  events: StreamEvent[];
}

const RECONNECT_DELAYS = [500, 1000, 2000, 5000, 10000];
const FLUSH_INTERVAL_MS = 16;

/**
 * Subscribe to the complete session trace, including late delegated events.
 *
 * This is intentionally separate from `useSSE`: a turn stream ends with the turn, while
 * a session trace stays alive across turns. The trace endpoint replays stable history and
 * a bounded live projection, so reconnecting never requires guessing which turn owns the
 * next frame.
 */
export function useSessionTrace(
  sessionId: string | undefined,
  enabled = true,
  snapshotCursor = -1,
): { events: StreamEvent[]; status: SessionTraceStatus } {
  const [state, setState] = useState<SessionTraceState>({ sessionId, events: [] });
  const [connection, setConnection] = useState<Exclude<SessionTraceStatus, "closed">>(
    "connecting",
  );
  const [unsupportedSession, setUnsupportedSession] = useState<string | null>(null);
  const cursorRef = useRef(-1);

  useEffect(() => {
    if (!sessionId || !enabled) return;

    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let flushTimer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    const controller = new AbortController();
    let queue: StreamEvent[] = [];

    // The returned projection is keyed by `sessionId`, so an old session's events are
    // hidden during navigation without a render-phase or effect-phase state reset. The
    // first flush below adopts the new key; this avoids both a stale frame and the
    // cascading render that a synchronous setState inside this effect would cause.
    cursorRef.current = snapshotCursor;

    function flush() {
      flushTimer = undefined;
      if (cancelled || queue.length === 0) return;
      const batch = queue;
      queue = [];
      setState((previous) => {
        const prior = previous.sessionId === sessionId
          ? previous.events.filter((event) => event.seq > snapshotCursor)
          : [];
        const seen = new Set(prior.map((event) => event.seq));
        const next = [...prior];
        for (const event of batch) {
          if (!seen.has(event.seq)) {
            seen.add(event.seq);
            next.push(event);
          }
        }
        next.sort((a, b) => a.seq - b.seq);
        return { sessionId, events: next };
      });
    }

    function enqueue(event: StreamEvent) {
      queue.push(event);
      flushTimer ??= setTimeout(flush, FLUSH_INTERVAL_MS);
    }

    async function connect() {
      if (cancelled) return;
      try {
        const response = await fetch(
          `/api/v1/sessions/${segment(sessionId!)}/trace/events?after_seq=${cursorRef.current}`,
          {
            credentials: "include",
            headers: { Accept: "text/event-stream" },
            cache: "no-store",
            signal: controller.signal,
          },
        );
        // A deleted session has no trace to reconnect to.
        if (response.status === 404) {
          setUnsupportedSession(sessionId!);
          return;
        }
        if (!response.ok || !response.body) throw new Error(`trace stream ${response.status}`);
        attempt = 0;
        setConnection("open");
        const reader = response.body.getReader();
        const utf8 = new TextDecoder();
        const decoder = createSSEDecoder();

        for (;;) {
          const { done, value } = await reader.read();
          if (done || cancelled) break;
          for (const frame of decoder.push(utf8.decode(value, { stream: true }))) {
            let event: StreamEvent;
            try {
              event = JSON.parse(frame.data) as StreamEvent;
            } catch {
              continue;
            }
            if (typeof event.seq !== "number" || event.seq <= cursorRef.current) continue;
            cursorRef.current = event.seq;
            enqueue(event);
          }
        }
        flush();
      } catch {
        if (cancelled || controller.signal.aborted) return;
      }
      if (cancelled) return;
      flush();
      setConnection("reconnecting");
      timer = setTimeout(() => void connect(), RECONNECT_DELAYS[Math.min(attempt++, 4)]);
    }

    void connect();
    return () => {
      cancelled = true;
      clearTimeout(timer);
      clearTimeout(flushTimer);
      controller.abort();
    };
  }, [sessionId, enabled, snapshotCursor]);

  return {
    events: state.sessionId === sessionId
      ? state.events.filter((event) => event.seq > snapshotCursor)
      : [],
    status:
      !sessionId || !enabled
        ? "closed"
        : state.sessionId !== sessionId
          ? "connecting"
          : unsupportedSession === sessionId
            ? "closed"
            : connection,
  };
}
