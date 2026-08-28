"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { StreamEvent } from "@/lib/api";
import { createSSEDecoder } from "@/lib/sse-parse";

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

/**
 * How long to batch arriving events before repainting.
 *
 * A streaming agent emits deltas at token rate — the claude runner measured at
 * ~20ms between tool-input deltas — and one `setState` per delta means one React
 * commit per token. Coalescing into a single frame per animation tick keeps the
 * repaint rate bounded by the display rather than by the model's speed, while
 * staying far below the ~100ms at which a human perceives lag.
 *
 * The flush is unconditional in the sense that no event waits longer than this:
 * a lone final event still lands one tick later, not never.
 */
const FLUSH_INTERVAL_MS = 16;

/** Buffered events tagged with the turn they belong to. */
interface Buffer {
  turnId: string | null;
  events: StreamEvent[];
}

/**
 * Subscribe to a turn's event stream, resuming after a drop.
 *
 * # Why `fetch` rather than `EventSource`
 *
 * The backend names every SSE frame after its event kind (`event: thinking`,
 * `event: tool_call`), because that is what `docs/api-design.md` specifies and
 * what makes the raw stream readable with `curl`. `EventSource` dispatches by
 * that name, which makes it the wrong tool three times over:
 *
 * 1. `onmessage` receives *only* frames named `message`. Listening there against
 *    a named stream drops every event — the bug this hook was rewritten to fix,
 *    whose symptom was a transcript that appeared all at once when the turn
 *    ended, because the only repaint was the history refetch on completion.
 * 2. A frame named `error` — which the backend emits for a failed turn — arrives
 *    as the same `error` event the browser fires for a dropped socket, so the two
 *    cannot be told apart.
 * 3. One listener per known kind silently drops any kind added later. Silent loss
 *    is the worst outcome available: it renders identically to an agent that did
 *    nothing.
 *
 * Reading the body ourselves fixes all three, and buys two things `EventSource`
 * cannot offer: cancellation through `AbortController` (so a teardown is
 * immediate rather than waiting on the browser's own retry), and an explicit
 * cursor on reconnect. `EventSource` reconnects on its own with no way to
 * advance `after_seq`, which is why the socket is managed explicitly here.
 *
 * Cost of the swap: the browser's automatic retry is gone, so the backoff below
 * is load-bearing rather than belt-and-braces.
 */
export function useSSE({ turnId, afterSeq = -1, enabled = true }: UseSSEOptions) {
  const [buffer, setBuffer] = useState<Buffer>({ turnId, events: [] });
  const [connection, setConnection] = useState<Exclude<SSEStatus, "closed">>(
    "connecting",
  );
  const cursorRef = useRef(afterSeq);
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
    let flushTimer: ReturnType<typeof setTimeout> | undefined;
    let cancelled = false;
    const controller = new AbortController();

    /** Events decoded since the last repaint. */
    let queue: StreamEvent[] = [];

    function flush() {
      flushTimer = undefined;
      if (cancelled || queue.length === 0) return;
      const batch = queue;
      queue = [];
      setBuffer((prev) =>
        prev.turnId === turnId ? { turnId, events: [...prev.events, ...batch] } : prev,
      );
    }

    function enqueue(event: StreamEvent) {
      queue.push(event);
      flushTimer ??= setTimeout(flush, FLUSH_INTERVAL_MS);
    }

    async function connect() {
      if (cancelled) return;

      try {
        const response = await fetch(
          // Raw interpolation is safe *here specifically*: a turn id is a server-minted
          // surrogate key with no `:` or `/` in it. It is not safe for a session id, which
          // is the identity triple — see `segment` in `lib/api.ts`, and the sibling stream
          // in `use-agent-session.ts` that had to adopt it. Do not copy this line for an
          // id type without checking what characters that type can hold.
          `/api/v1/turns/${turnId}/events?after_seq=${cursorRef.current}`,
          {
            credentials: "include",
            headers: { accept: "text/event-stream" },
            signal: controller.signal,
            // The turn outlives any one request, so a cached response would be
            // meaningless as well as stale.
            cache: "no-store",
          },
        );

        if (!response.ok || !response.body) {
          throw new Error(`stream responded ${response.status}`);
        }

        if (cancelled) return;
        attemptRef.current = 0;
        setConnection("open");

        const reader = response.body.getReader();
        // `stream: true` matters: a chunk boundary can fall inside a multi-byte
        // UTF-8 sequence, and a stateless decode would emit a replacement
        // character for a perfectly good CJK character or emoji.
        const utf8 = new TextDecoder();
        const decoder = createSSEDecoder();

        for (;;) {
          const { done, value } = await reader.read();
          if (done || cancelled) break;

          for (const frame of decoder.push(utf8.decode(value, { stream: true }))) {
            let event: StreamEvent;
            try {
              event = JSON.parse(frame.data) as StreamEvent;
            } catch (parseError) {
              // One malformed frame must not kill a live stream; the rest of the
              // turn is still worth rendering.
              console.error("Malformed SSE payload:", parseError, frame.data);
              continue;
            }

            // The cursor is the contract: an event at or below it was already
            // rendered, so a replay overlap after reconnect is dropped here.
            if (typeof event.seq !== "number" || event.seq <= cursorRef.current) {
              continue;
            }
            cursorRef.current = event.seq;
            enqueue(event);
          }
        }

        // A clean end-of-body means the server closed the stream, which it does
        // when the turn is finished. Retrying would reconnect, replay nothing,
        // and close again in a loop, so this settles instead.
        flush();
        if (!cancelled) setConnection("open");
        return;
      } catch (error) {
        // An abort is our own teardown, not a failure.
        if (cancelled || controller.signal.aborted) return;
        console.error("Turn stream dropped:", error);
      }

      flush();
      setConnection("reconnecting");
      const delay =
        RECONNECT_DELAYS[Math.min(attemptRef.current, RECONNECT_DELAYS.length - 1)];
      attemptRef.current += 1;
      timer = setTimeout(() => void connect(), delay);
    }

    void connect();

    return () => {
      cancelled = true;
      clearTimeout(timer);
      clearTimeout(flushTimer);
      controller.abort();
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
