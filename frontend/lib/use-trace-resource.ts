"use client";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ApiError } from "./api";
import {
  clearTraceAccess,
  invalidateTraceView,
  onTraceAccessCleared,
  traceMetadata,
} from "./trace-cache";
import { readTraceWhenReady } from "./trace-get";
import {
  discardUnusedTraceView,
  retainTraceView,
  viewOfTraceResponse,
} from "./trace-leases";

/** Each mounted surface owns cancellation and keeps its previous visible result. */
export function useTraceResource<T>(
  key: string | null,
  read: (signal: AbortSignal) => Promise<T>,
  refresh = 0,
  cache = true,
) {
  const latest = useRef(read);
  const heldView = useRef<(() => void) | null>(null);
  useLayoutEffect(() => {
    latest.current = read;
  });
  const [state, setState] = useState<{
    key: string | null;
    data?: T;
    error?: Error;
    pending: boolean;
  }>({ key, pending: Boolean(key) });
  useEffect(() => {
    if (!key) {
      heldView.current?.();
      heldView.current = null;
      return;
    }
    const controller = new AbortController();
    const cached = cache
      ? (traceMetadata.get(key) as T | undefined)
      : undefined;
    const clear = onTraceAccessCleared((reason) => {
      controller.abort();
      setState({
        key,
        pending: false,
        error: new Error(
          reason === "view"
            ? "This retained view expired. Refreshing around the current item."
            : "Access changed. Open this conversation again to refresh authorization.",
        ),
      });
    });
    void Promise.resolve().then(() => {
      if (!controller.signal.aborted)
        setState((previous) => ({
          key,
          data: cached ?? (previous.key === key ? previous.data : undefined),
          pending: true,
        }));
    });
    readTraceWhenReady(
      () => latest.current(controller.signal),
      controller.signal,
    ).then(
      (data) => {
        const view = viewOfTraceResponse(data);
        if (controller.signal.aborted) {
          if (view) discardUnusedTraceView(view);
          return;
        }
        const release = view ? retainTraceView(view) : null;
        heldView.current?.();
        heldView.current = release;
        if (cache) {
          const bytes = new TextEncoder().encode(JSON.stringify(data)).length;
          traceMetadata.set(key, data, bytes);
        }
        setState({ key, data, pending: false });
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        if (
          error instanceof ApiError &&
          (error.status === 401 || error.status === 403)
        )
          clearTraceAccess();
        if (error instanceof ApiError && error.status === 410)
          invalidateTraceView();
        setState((previous) => ({
          key,
          data: previous.key === key ? previous.data : undefined,
          error: error instanceof Error ? error : new Error(String(error)),
          pending: false,
        }));
      },
    );
    return () => {
      clear();
      controller.abort();
    };
  }, [key, refresh, cache]);
  useEffect(
    () => () => {
      heldView.current?.();
      heldView.current = null;
    },
    [],
  );
  return state.key === key
    ? state
    : { key, pending: Boolean(key), data: undefined, error: undefined };
}
