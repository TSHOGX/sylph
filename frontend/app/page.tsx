"use client";

import { useQuery } from "@tanstack/react-query";
import Link from "next/link";
import { useMemo, useState } from "react";
import { api, type SessionFilters, type SessionOrigin } from "@/lib/api";
import { formatRelative, sessionLabel } from "@/lib/format";
import { useDebounced } from "@/lib/use-debounced";
import { OriginBadge, SessionStatusBadge } from "@/components/badges";

interface Filters {
  agent: string;
  origin: string;
  workdir: string;
  since: string;
}

const EMPTY_FILTERS: Filters = { agent: "", origin: "", workdir: "", since: "" };

/**
 * Translate form state into backend query parameters.
 *
 * Filtering runs server-side because `GET /api/v1/sessions` is `LIMIT`-capped:
 * narrowing an already-truncated page would silently hide older matches that
 * never left the database. Blank fields are dropped so they act as no-ops.
 */
function toQuery(filters: Filters): SessionFilters {
  const sinceMs = filters.since ? new Date(filters.since).getTime() : NaN;

  return {
    agent: filters.agent || undefined,
    origin: (filters.origin || undefined) as SessionOrigin | undefined,
    workdir: filters.workdir.trim() || undefined,
    updatedAfterMs: Number.isFinite(sinceMs) ? sinceMs : undefined,
  };
}

export default function HomePage() {
  const [filters, setFilters] = useState<Filters>(EMPTY_FILTERS);
  const filtersActive = Object.values(filters).some(Boolean);

  // Only the free-text box needs debouncing; selects and dates settle on their own.
  const debouncedWorkdir = useDebounced(filters.workdir);
  const query = useMemo(
    () => toQuery({ ...filters, workdir: debouncedWorkdir }),
    [filters, debouncedWorkdir],
  );

  const { data, isPending, error, refetch, isFetching } = useQuery({
    queryKey: ["sessions", query],
    queryFn: () => api.sessions.list(query),
    // Keep the previous page visible while a filter change is in flight, so the
    // list dims rather than flashing an empty state on every keystroke.
    placeholderData: (previous) => previous,
  });

  /**
   * The agent dropdown is built from the unfiltered listing. Deriving it from
   * filtered results would collapse the menu to the agent already chosen, making
   * it impossible to switch to another one.
   */
  const { data: unfiltered } = useQuery({
    queryKey: ["sessions", {}],
    queryFn: () => api.sessions.list(),
  });

  const sessions = useMemo(() => data?.sessions ?? [], [data]);
  const agents = useMemo(
    () => Array.from(new Set((unfiltered?.sessions ?? []).map((s) => s.agent))).sort(),
    [unfiltered],
  );
  const total = unfiltered?.sessions.length ?? 0;
  const visible = sessions;

  return (
    <div className="min-h-screen bg-white">
      <header className="border-b">
        <div className="container mx-auto flex flex-wrap items-center justify-between gap-3 px-4 py-4">
          <h1 className="text-xl font-semibold">Sylph Web</h1>
          <nav className="flex items-center gap-2">
            <Link
              href="/search"
              className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Search archive
            </Link>
            <Link
              href="/sessions/new"
              className="rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              New session
            </Link>
          </nav>
        </div>
      </header>

      <main className="container mx-auto px-4 py-6">
        {isPending ? (
          <output
            aria-live="polite"
            className="block py-12 text-center text-sm text-gray-500"
          >
            Loading sessions...
          </output>
        ) : error ? (
          <div
            role="alert"
            className="mx-auto max-w-md rounded-lg border border-red-200 bg-red-50 p-4 text-center"
          >
            <p className="font-medium text-red-900">Could not load sessions</p>
            <p className="mt-1 text-sm text-red-800">
              {error instanceof Error ? error.message : "Unknown error."}
            </p>
            <button
              type="button"
              onClick={() => void refetch()}
              disabled={isFetching}
              className="mt-3 rounded border border-red-300 bg-white px-3 py-1.5 text-sm font-medium hover:bg-red-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600 disabled:opacity-50"
            >
              {isFetching ? "Retrying..." : "Retry"}
            </button>
          </div>
        ) : total === 0 && !filtersActive ? (
          <div className="py-16 text-center">
            <p className="font-medium text-gray-800">No sessions yet</p>
            <p className="mx-auto mt-1 max-w-sm text-sm text-gray-500">
              A session pairs an agent with a working directory. Create one to start a
              conversation, or search the TraceDB archive to import existing history.
            </p>
            <Link
              href="/sessions/new"
              className="mt-4 inline-block rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Create your first session
            </Link>
          </div>
        ) : (
          <>
            <FilterBar
              filters={filters}
              agents={agents}
              onChange={setFilters}
              onReset={() => setFilters(EMPTY_FILTERS)}
              showing={visible.length}
              total={total}
              busy={isFetching}
            />

            {visible.length === 0 ? (
              <div className="py-12 text-center">
                <p className="font-medium text-gray-800">
                  No sessions match these filters
                </p>
                <button
                  type="button"
                  onClick={() => setFilters(EMPTY_FILTERS)}
                  className="mt-3 rounded-lg border px-4 py-2 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                >
                  Clear filters
                </button>
              </div>
            ) : (
              <ul className="mt-4 grid gap-3">
                {visible.map((session) => (
                  <li key={session.id}>
                    <Link
                      href={`/sessions/${session.id}`}
                      className="block rounded-lg border p-4 transition hover:border-gray-400 hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                    >
                      <div className="flex flex-wrap items-start justify-between gap-2">
                        <h2 className="font-medium">{sessionLabel(session)}</h2>
                        <div className="flex shrink-0 items-center gap-1.5">
                          <OriginBadge origin={session.origin} />
                          <SessionStatusBadge status={session.status} />
                        </div>
                      </div>
                      <p className="mt-1 truncate text-sm text-gray-500">
                        {session.agent} ·{" "}
                        <span className="font-mono text-xs">{session.workdir}</span>
                      </p>
                      <p className="mt-1 text-xs text-gray-400">
                        updated {formatRelative(session.updatedAtMs)}
                      </p>
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </main>
    </div>
  );
}

function FilterBar({
  filters,
  agents,
  onChange,
  onReset,
  showing,
  total,
  busy,
}: {
  filters: Filters;
  agents: string[];
  onChange: (next: Filters) => void;
  onReset: () => void;
  showing: number;
  total: number;
  busy: boolean;
}) {
  const active = Object.values(filters).some(Boolean);

  return (
    <section aria-label="Filter sessions" className="rounded-lg border bg-gray-50 p-3">
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <div>
          <label htmlFor="filter-agent" className="block text-xs font-medium text-gray-600">
            Agent
          </label>
          <select
            id="filter-agent"
            value={filters.agent}
            onChange={(e) => onChange({ ...filters, agent: e.target.value })}
            className="mt-1 w-full rounded border bg-white px-2 py-1.5 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            <option value="">Any agent</option>
            {agents.map((agent) => (
              <option key={agent} value={agent}>
                {agent}
              </option>
            ))}
          </select>
        </div>

        <div>
          <label htmlFor="filter-origin" className="block text-xs font-medium text-gray-600">
            Origin
          </label>
          <select
            id="filter-origin"
            value={filters.origin}
            onChange={(e) => onChange({ ...filters, origin: e.target.value })}
            className="mt-1 w-full rounded border bg-white px-2 py-1.5 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            <option value="">Any origin</option>
            <option value="sylph">Sylph</option>
            <option value="native">Native</option>
          </select>
        </div>

        <div>
          <label htmlFor="filter-workdir" className="block text-xs font-medium text-gray-600">
            Workspace contains
          </label>
          <input
            id="filter-workdir"
            type="search"
            value={filters.workdir}
            onChange={(e) => onChange({ ...filters, workdir: e.target.value })}
            placeholder="/Users/you/project"
            className="mt-1 w-full rounded border bg-white px-2 py-1.5 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          />
        </div>

        <div>
          <label htmlFor="filter-since" className="block text-xs font-medium text-gray-600">
            Updated on or after
          </label>
          <input
            id="filter-since"
            type="date"
            value={filters.since}
            onChange={(e) => onChange({ ...filters, since: e.target.value })}
            className="mt-1 w-full rounded border bg-white px-2 py-1.5 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          />
        </div>
      </div>

      <div className="mt-2 flex items-center justify-between">
        <p aria-live="polite" className="text-xs text-gray-500">
          {busy
            ? "Filtering..."
            : `Showing ${showing} of ${total} ${total === 1 ? "session" : "sessions"}`}
        </p>
        {active && (
          <button
            type="button"
            onClick={onReset}
            className="text-xs font-medium text-gray-700 underline hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            Clear filters
          </button>
        )}
      </div>
    </section>
  );
}
