"use client";
import { useState } from "react";
import type { TracePage } from "./trace-types";
import { tracePageButtonDisabled } from "./trace-window";
import { useTraceResource } from "./use-trace-resource";

/** One visible detail page, with the failed cursor retained until it succeeds. */
export function useTracePage<T>(
  scope: string,
  firstCursor: string | null,
  read: (cursor: string, signal: AbortSignal) => Promise<TracePage<T>>,
) {
  const [requested, setRequested] = useState({
    scope,
    cursor: null as string | null,
    attempt: 0,
  });
  const cursor = requested.scope === scope ? requested.cursor : null;
  // The scope includes the immutable view and every query parameter except the
  // page cursor. Keeping that key stable preserves the last successful page while
  // a later page fails. This local page never populates the shared metadata cache.
  const result = useTraceResource(
    cursor === null ? null : scope,
    (signal) => read(cursor!, signal),
    requested.attempt,
    false,
  );
  const disabled =
    tracePageButtonDisabled(
      result.pending,
      Boolean(result.error),
      result.data?.hasAfter,
      cursor !== null,
    ) ||
    (cursor === null && firstCursor === null);
  const load = () => {
    if (disabled) return;
    const next = result.error
      ? cursor
      : (result.data?.afterCursor ?? firstCursor);
    if (next === null) return;
    // A cursor is a position, not a request identity: retrying it must still cause
    // a new GET. It must not derive another position from the preceding good page.
    setRequested((previous) => ({
      scope,
      cursor: next,
      attempt: previous.attempt + 1,
    }));
  };
  return { ...result, disabled, load };
}
