"use client";

/**
 * The one query for live sessions, shared by every surface that lists them.
 *
 * The sidebar and the start screen show the same rows for different reasons —
 * navigation and resumption — and both mount together on `/`. Declaring the query
 * in one place makes that a single request rather than two: TanStack dedupes by
 * key, so the second consumer reads the first one's cache entry. Two hand-written
 * key literals would only dedupe as long as nobody changed one of the limits, and
 * the failure mode of that drift is silent — a duplicate request per poll, plus a
 * start screen listing different sessions than the sidebar beside it.
 */

import { queryOptions } from "@tanstack/react-query";
import { api } from "@/lib/api";

/**
 * Sessions fetched at once. Higher than the API default: the sidebar is the
 * primary navigation surface, so truncating it hides work rather than paging it.
 */
export const LIVE_SESSIONS_LIMIT = 200;

export function liveSessionsQuery() {
  return queryOptions({
    queryKey: ["sessions", { limit: LIVE_SESSIONS_LIMIT }],
    queryFn: () => api.sessions.list({ limit: LIVE_SESSIONS_LIMIT }),
    // Running state changes without any action on this client, so both consumers
    // need it refreshed rather than only revalidated on focus.
    refetchInterval: 10_000,
  });
}
