"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { StreamEvent } from "@/lib/api";

export type { StreamEvent };

export type SSEStatus = "connecting" | "open" | "reconnecting" | "closed";

interface UseSSEOptions {
  /** Turn to stream. Pass null to hold the connection closed. */
  turnId: string | null;
  /** Replay resumes strictly after this seq; -1 replays the whole journal. */
  afterSeq?: number;
  /** Stop connecting once the turn is known to be finished. */
  enabled?: boolean;
}

/** Backoff schedule in ms. Caps out so a long-dead backend is polled gently. */
const RECONNECT_DELAYS = [500, 1000, 2000, 5000, 10000];

/** Buffered events tagged with the turn they belong to. */
interface Buffer {
  turnId: string | null;
  events: StreamEvent[];
}

/**
 * Subscribe to a turn's event stream, resuming after a drop.
 *
 * Reconnection is why `cursorRef` exists: the backend replays the journal from
 * `after_seq`, so tracking the highest seq seen means a dropped connection
 * resumes without duplicating or losing events. EventSource retries on its own
 * only for network-level closes and offers no way to advance that cursor, so
 * the socket is closed and reopened explicitly.
 */
export function useSSE({ turnId, afterSeq = -1, enabled = true }: UseSSEOptions) {
  const [buffer, setBuffer] = useState<Buffer>({ turnId, events: [] });
  const [connection, setConnection] = useState<Exclude<SSEStatus, "closed">>(
    "connecting",
  );
  const cursorRef = useRef(afterSeq);
  const sourceRef = useRef<EventSource | null>(null);
  const attemptRef = useRef(0);

  // A new turn is a new stream, so its buffer starts empty. This is the
  // render-phase adjustment React prescribes for state derived from a changing
  // input; doing it in an effect would paint one frame of the previous turn's
  // events under the new turn's heading. Refs are untouchable during render,
  // so the cursor resets in the effect below instead.
  if (buffer.turnId !== turnId) {
    setBuffer({ turnId, events: [] });
    setConnection("connecting");
  }

  useEffect(() => {
    if (!turnId || !enabled) return;

    // Same trigger as the buffer reset above, but here refs are safe to write.
    cursorRef.current = afterSeq;
    attemptRef.current = 0;

    let timer: ReturnType<typeof setTimeout> | undefined;
    let cancelled = false;

    function connect() {
      if (cancelled) return;

      const source = new EventSource(
        `/api/v1/turns/${turnId}/events?after_seq=${cursorRef.current}`,
        { withCredentials: true },
      );
      sourceRef.current = source;

      source.onopen = () => {
        attemptRef.current = 0;
        setConnection("open");
      };

      source.onmessage = (message) => {
        try {
          const event = JSON.parse(message.data) as StreamEvent;
          // The cursor is the contract: an event at or below it was already
          // rendered, so a replay overlap after reconnect is dropped here.
          if (event.seq <= cursorRef.current) return;
          cursorRef.current = event.seq;
          setBuffer((prev) =>
            prev.turnId === turnId
              ? { turnId, events: [...prev.events, event] }
              : prev,
          );
        } catch (parseError) {
          console.error("Malformed SSE payload:", parseError, message.data);
        }
      };

      source.onerror = () => {
        source.close();
        sourceRef.current = null;
        if (cancelled) return;

        setConnection("reconnecting");
        const delay =
          RECONNECT_DELAYS[
            Math.min(attemptRef.current, RECONNECT_DELAYS.length - 1)
          ];
        attemptRef.current += 1;
        timer = setTimeout(connect, delay);
      };
    }

    connect();

    return () => {
      cancelled = true;
      clearTimeout(timer);
      sourceRef.current?.close();
      sourceRef.current = null;
    };
    // `afterSeq` is deliberately not a dependency. It is the cursor to start a
    // new turn's stream from, read once per connection; adding it would tear
    // down and rewind a healthy live stream whenever a caller re-rendered with
    // a new value, replaying events the user has already seen.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [turnId, enabled]);

  // Derived rather than stored: with no turn or while disabled there is nothing
  // to connect to, so "closed" is a fact about the inputs.
  const status: SSEStatus = !turnId || !enabled ? "closed" : connection;

  const clear = useCallback(
    () => setBuffer((prev) => ({ turnId: prev.turnId, events: [] })),
    [],
  );

  return {
    events: buffer.turnId === turnId ? buffer.events : [],
    status,
    isConnected: status === "open",
    clear,
  };
}
