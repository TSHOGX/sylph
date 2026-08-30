"use client";

/**
 * The one query for workspaces, shared by every surface that needs them.
 *
 * Eight surfaces read this list — the shell (for `quickStartDir`), the sidebar, the start
 * screen, the settings panel, and four pickers — and several mount together. Declaring the
 * query once makes that one request rather than eight: TanStack dedupes by key, so every
 * later consumer reads the first one's cache entry.
 *
 * # Why one declaration rather than eight agreeing ones
 *
 * Hand-written literals dedupe only while every copy agrees, and the drift is silent.
 * These had already diverged into two `staleTime` values, and **four of them were dead**:
 * `AppShell` sits in the root layout, so a 30-second observer is mounted on every route,
 * and TanStack resolves staleness per observer — the shortest wins. The four 60-second
 * declarations could never take effect, which is the shape of drift that reads as a
 * deliberate choice while doing nothing.
 *
 * Same reasoning as `lib/sessions-query.ts`, which owns the sessions list for the same
 * reason.
 */

import { queryOptions } from "@tanstack/react-query";
import { api } from "@/lib/api";

/**
 * How long a workspace listing stays fresh.
 *
 * A workspace appears when a session is created in a new directory, and every path that
 * does so invalidates this key — so the interval is a backstop against a change this
 * client did not make, not the mechanism that keeps the list current.
 */
const WORKSPACES_STALE_MS = 30_000;

export function workspacesQuery() {
  return queryOptions({
    queryKey: ["workspaces"],
    queryFn: () => api.workspaces.list(),
    staleTime: WORKSPACES_STALE_MS,
  });
}
