"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { ApiError, api, type SearchResult } from "@/lib/api";
import { formatRelative } from "@/lib/format";
import { useDebounced } from "@/lib/use-debounced";

/**
 * Search the TraceDB archive and adopt results as sessions.
 *
 * Ranking and fork-lineage collapsing belong to TraceDB, so results are rendered
 * in the order returned; this page adds no scoring of its own.
 */
export default function SearchPage() {
  const [rawQuery, setRawQuery] = useState("");
  const query = useDebounced(rawQuery, 300);
  const trimmed = query.trim();

  const { data, isFetching, error } = useQuery({
    queryKey: ["search", trimmed],
    queryFn: () => api.search(trimmed),
    // A blank box is not a search: the backend short-circuits it, but skipping
    // the request keeps the empty state from flickering while typing.
    enabled: trimmed.length > 0,
    placeholderData: (previous) => previous,
  });

  const results = data?.results ?? [];
  const archiveDown = data ? !data.available : false;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-gray-200 px-4 py-2">
        <h1 className="text-sm font-semibold">Search archive</h1>
        <Link
          href="/"
          className="rounded-md border border-gray-200 px-2.5 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          Start screen
        </Link>
      </header>

      <div className="mx-auto max-w-4xl px-4 py-6">
        <form role="search" onSubmit={(e) => e.preventDefault()}>
          <label htmlFor="archive-query" className="block text-sm font-medium">
            Find archived coding-agent sessions
          </label>
          <input
            id="archive-query"
            type="search"
            value={rawQuery}
            onChange={(e) => setRawQuery(e.target.value)}
            autoFocus
            placeholder="error handling, sqlx migration, ..."
            aria-describedby="archive-query-hint"
            className="mt-1 w-full rounded-lg border px-3 py-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          />
          <p id="archive-query-hint" className="mt-1 text-xs text-gray-500">
            Searches history recorded by Claude Code, Codex, and OpenCode outside
            Sylph Web. Importing adopts a session without altering what was recorded.
          </p>
        </form>

        <div aria-live="polite" aria-busy={isFetching} className="mt-6">
          {error ? (
            <ErrorPanel message={describeError(error)} />
          ) : archiveDown ? (
            <ArchiveUnavailable detail={data?.detail} />
          ) : trimmed.length === 0 ? (
            <p className="py-12 text-center text-sm text-gray-500">
              Type a query to search the archive.
            </p>
          ) : isFetching && results.length === 0 ? (
            <p className="py-12 text-center text-sm text-gray-500">Searching...</p>
          ) : results.length === 0 ? (
            <div className="py-12 text-center">
              <p className="font-medium text-gray-800">
                No archived sessions match “{trimmed}”
              </p>
              <p className="mx-auto mt-1 max-w-sm text-sm text-gray-500">
                Try a shorter query, or a phrase you remember typing at the time.
              </p>
            </div>
          ) : (
            <>
              <p className="text-xs text-gray-500">
                {results.length} {results.length === 1 ? "session" : "sessions"}
                {isFetching && " · updating"}
              </p>
              <ul className="mt-3 grid gap-3">
                {results.map((hit) => (
                  <li key={hit.id}>
                    <ResultCard hit={hit} />
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function ResultCard({ hit }: { hit: SearchResult }) {
  const router = useRouter();
  const queryClient = useQueryClient();

  const importSession = useMutation({
    mutationFn: () => api.sessions.import({ traceSessionId: hit.id }),
    onSuccess: async ({ session }) => {
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });
      await queryClient.invalidateQueries({ queryKey: ["search"] });
      router.push(`/sessions/${session.id}`);
    },
  });

  const label = hit.title || hit.ask || hit.id;
  const snippet = hit.bestMatch?.snippet;

  return (
    <article className="rounded-lg border p-4">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <h2 className="font-medium">{label}</h2>
        <span className="shrink-0 rounded-full bg-violet-100 px-2 py-0.5 text-xs font-medium text-violet-800">
          {hit.agent}
        </span>
      </div>

      {snippet && (
        <p className="mt-2 border-l-2 border-gray-200 pl-3 text-sm text-gray-600">
          {snippet}
        </p>
      )}

      <p className="mt-2 truncate text-xs text-gray-500">
        {hit.cwd && <span className="font-mono">{hit.cwd}</span>}
        {hit.startedAtMs != null && <> · started {formatRelative(hit.startedAtMs)}</>}
        {hit.hits != null && (
          <>
            {" "}
            · {hit.hits} matching {hit.hits === 1 ? "event" : "events"}
          </>
        )}
      </p>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        {hit.sessionId ? (
          <Link
            href={`/sessions/${hit.sessionId}`}
            className="rounded-lg border px-3 py-1.5 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            Open session
          </Link>
        ) : (
          <button
            type="button"
            onClick={() => importSession.mutate()}
            disabled={importSession.isPending}
            className="rounded-lg bg-gray-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
          >
            {importSession.isPending ? "Importing..." : "Import session"}
          </button>
        )}
        <span className="text-xs text-gray-500">
          {hit.sessionId ? "Already imported" : "Imported as a native session"}
        </span>
      </div>

      {importSession.error && (
        <p role="alert" className="mt-2 text-sm text-red-800">
          {describeImportError(importSession.error)}
        </p>
      )}
    </article>
  );
}

/**
 * Turn the backend's import error codes into something actionable.
 *
 * Each of these has a different remedy, so they must not collapse into one
 * "import failed" message.
 */
function describeImportError(error: unknown): string {
  if (!(error instanceof ApiError)) return describeError(error);

  switch (error.code) {
    case "SESSION_ALREADY_IMPORTED":
      return "This session is already imported. Reload to see it in the list.";
    case "TRACE_SESSION_NOT_FOUND":
      return "The archive no longer has this session. It may have been pruned.";
    case "TRACEDB_UNAVAILABLE":
      return "The archive became unreachable during import. Try again once trace-db is running.";
    case "TRACE_SESSION_NOT_RESUMABLE":
      return "The archive recorded no working directory for this session, so there is no directory to resume it in. Sessions archived by older trace-db versions may be missing this.";
    default:
      return error.message;
  }
}

function describeError(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return error instanceof Error ? error.message : "Unknown error.";
}

function ErrorPanel({ message }: { message: string }) {
  return (
    <div role="alert" className="rounded-lg border border-red-200 bg-red-50 p-4">
      <p className="font-medium text-red-900">Search failed</p>
      <p className="mt-1 text-sm text-red-800">{message}</p>
    </div>
  );
}

/**
 * Shown when the request succeeded but TraceDB could not be reached.
 *
 * This is deliberately distinct from "no matches": the archive being down is a
 * fixable local condition, and reporting it as an empty result would imply the
 * history is gone.
 */
function ArchiveUnavailable({ detail }: { detail?: string }) {
  return (
    <div role="alert" className="rounded-lg border border-amber-200 bg-amber-50 p-4">
      <p className="font-medium text-amber-900">Archive unavailable</p>
      <p className="mt-1 text-sm text-amber-800">
        The TraceDB archive could not be reached, so past sessions cannot be
        searched right now. Live sessions are unaffected. Check that the{" "}
        <code className="font-mono text-xs">trace-db</code> binary is installed and
        that <code className="font-mono text-xs">TRACEDB_BIN</code> points at it.
      </p>
      {detail && <p className="mt-2 font-mono text-xs text-amber-700">{detail}</p>}
    </div>
  );
}
