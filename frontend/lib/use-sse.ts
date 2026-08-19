"use client";

import { useEffect, useRef, useState } from "react";

export interface StreamEvent {
  seq: number;
  kind: string;
  text?: string;
  phase?: string;
  data?: unknown;
  is_error?: boolean;
  created_at_ms: number;
}

interface UseSSEOptions {
  turnId: string;
  onEvent?: (event: StreamEvent) => void;
  afterSeq?: number;
}

export function useSSE({ turnId, onEvent, afterSeq = -1 }: UseSSEOptions) {
  const [events, setEvents] = useState<StreamEvent[]>([]);
  const [error, setError] = useState<Error | null>(null);
  const [isConnected, setIsConnected] = useState(false);
  const eventSourceRef = useRef<EventSource | null>(null);
  const lastSeqRef = useRef(afterSeq);

  useEffect(() => {
    let reconnectTimeout: NodeJS.Timeout;

    function connect() {
      const url = `/api/v1/turns/${turnId}/events?after_seq=${lastSeqRef.current}`;
      const es = new EventSource(url, { withCredentials: true });

      es.onopen = () => {
        setIsConnected(true);
        setError(null);
      };

      es.onmessage = (e) => {
        try {
          const event: StreamEvent = JSON.parse(e.data);
          lastSeqRef.current = event.seq;

          setEvents((prev) => [...prev, event]);
          onEvent?.(event);
        } catch (err) {
          console.error("Failed to parse SSE event:", err);
        }
      };

      es.onerror = () => {
        setIsConnected(false);
        es.close();

        // Reconnect after 1s
        reconnectTimeout = setTimeout(() => {
          connect();
        }, 1000);
      };

      eventSourceRef.current = es;
    }

    connect();

    return () => {
      clearTimeout(reconnectTimeout);
      eventSourceRef.current?.close();
    };
  }, [turnId, onEvent]);

  return { events, error, isConnected };
}
