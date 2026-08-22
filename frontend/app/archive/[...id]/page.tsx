"use client";

/**
 * Read-only view of an archived session.
 *
 * Browsing history must not mutate anything, which is why this route exists
 * separately from importing. `POST /sessions/import` adopts a session so it can
 * be *continued*: it writes a row, requires a runner for the originating agent,
 * and requires the recorded working directory to still exist. None of that is
 * needed to read a transcript, and all of it fails for exactly the old sessions a
 * user most wants to look back at.
 *
 * So this page reads, and offers continuation as an explicit next step. The
 * timeline is the same component the live conversation uses — the backend
 * normalizes archived events into the live shape, so there is no second renderer
 * to drift.
 *
 * A catch-all route because a subagent session id contains a slash
 * (`claude:uuid/agent-name`), which is 43% of the local archive.
 */

import Link from "next/link";
import { useParams, useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, api } from "@/lib/api";
import { formatTimestamp } from "@/lib/format";
import { densityClasses, useSettings } from "@/lib/settings";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Timeline } from "@/components/timeline";

/** Events fetched at once. The largest archived session here is 8167 events. */
const EVENT_LIMIT = 2000;

export default function ArchivedSessionPage() {
  const params = useParams();
  const router = useRouter();
  const queryClient = useQueryClient();
  const { settings } = useSettings();
  const { showArtifact, setActiveWorkdir } = useWorkspaceUI();
  const [expandAll, setExpandAll] = useState<boolean | null>(null);

  // A catch-all yields the path segments still percent-encoded, so each is
  // decoded before rejoining with "/" to restore the archive's own spelling
  // (`claude:uuid`, or `claude:uuid/agent-name` for a subagent). Skipping the
  // decode sends `claude%3Auuid` to a client that encodes again, and the backend
  // then looks up a literal `claude%253Auuid`.
  const traceId = (Array.isArray(params.id) ? params.id : [params.id ?? ""])
    .map((segment) => decodeURIComponent(String(segment)))
    .join("/");

  const transcript = useQuery({
    queryKey: ["archive", "transcript", traceId],
    queryFn: () => api.archive.get(traceId, EVENT_LIMIT),
    enabled: traceId.length > 0,
  });

  const data = transcript.data;

  // Reading an archived session scopes the history browser to its directory, so
  // `Cmd+Shift+K` continues browsing where the user already is.
  useEffect(() => {
    if (data?.cwd) setActiveWorkdir(data.cwd);
  }, [data?.cwd, setActiveWorkdir]);

  const adopt = useMutation({
    mutationFn: () => api.sessions.import({ traceSessionId: traceId, title: data?.label }),
    onSuccess: ({ session }) => {
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      void queryClient.invalidateQueries({ queryKey: ["archive"] });
      router.push(`/sessions/${session.id}`);
    },
  });

  if (transcript.isPending) {
    return (
      <output
        aria-live="polite"
        className="flex flex-1 items-center justify-center text-sm text-gray-500"
      >
        Loading archived session...
      </output>
    );
  }

  if (transcript.error || !data) {
    const error = transcript.error;
    const missing = error instanceof ApiError && error.status === 404;
    const unavailable = error instanceof ApiError && error.code === "TRACEDB_UNAVAILABLE";
    return (
      <div role="alert" className="flex flex-1 items-center justify-center px-4">
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold text-red-800">
            {missing
              ? "Not in the archive"
              : unavailable
                ? "Archive unavailable"
                : "Could not load this session"}
          </h1>
          <p className="mt-2 text-sm text-gray-600">
            {missing
              ? "TraceDB has no session with this id. It may not have been ingested yet."
              : error instanceof Error
                ? error.message
                : "Unknown error."}
          </p>
          <div className="mt-4 flex justify-center gap-2">
            <button
              type="button"
              onClick={() => void transcript.refetch()}
              className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Retry
            </button>
            <Link
              href="/"
              className="rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Start screen
            </Link>
          </div>
        </div>
      </div>
    );
  }

  const density = densityClasses(settings.density);
  const title = data.label ?? `Untitled ${data.agent} session`;

  return (
    <>
      <header className="shrink-0 border-b border-gray-200 px-4 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="truncate text-sm font-semibold">{title}</h1>
          <span className="shrink-0 rounded bg-gray-100 px-1.5 py-0.5 text-[10px] font-medium text-gray-700">
            archived · read-only
          </span>
          <div className="ml-auto flex items-center gap-1">
            <button
              type="button"
              onClick={() => setExpandAll(expandAll === true ? null : true)}
              className="rounded px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Expand all
            </button>
            <button
              type="button"
              onClick={() => setExpandAll(expandAll === false ? null : false)}
              className="rounded px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Collapse all
            </button>
          </div>
        </div>
        <p className="mt-0.5 truncate text-[11px] text-gray-500">
          {data.agent}
          {data.cwd && (
            <>
              {" · "}
              <span className="font-mono">{data.cwd}</span>
            </>
          )}
          {data.startedAtMs ? ` · started ${formatTimestamp(data.startedAtMs)}` : ""}
        </p>
      </header>

      <ContinuationBar
        sessionId={data.sessionId}
        resumable={Boolean(data.cwd)}
        pending={adopt.isPending}
        error={adopt.error}
        onAdopt={() => adopt.mutate()}
      />

      {data.truncated && (
        <aside
          aria-label="Transcript truncated"
          className="shrink-0 border-b border-amber-200 bg-amber-50 px-4 py-1.5 text-[11px] text-amber-900"
        >
          Showing the first {EVENT_LIMIT.toLocaleString()} events of a longer session.
        </aside>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        <div className={`mx-auto max-w-3xl ${density.text}`}>
          {data.events.length === 0 ? (
            <p className="py-8 text-center text-sm text-gray-500">
              This archived session recorded no events.
            </p>
          ) : (
            <Timeline
              events={data.events}
              expandAll={expandAll}
              // Only meaningful once the session is live: the file endpoints are
              // rooted at a *session* workdir, and an archived session has no
              // session id to root them at.
              onOpenFile={
                data.sessionId
                  ? (path, line) =>
                      showArtifact({ sessionId: data.sessionId!, path, line })
                  : undefined
              }
            />
          )}
        </div>
      </div>
    </>
  );
}

/**
 * The one action this page offers: turn reading into continuing.
 *
 * Disabled rather than hidden when the archive recorded no working directory —
 * the import endpoint rejects those, and a button that always fails is worse than
 * one that explains why it cannot run.
 */
function ContinuationBar({
  sessionId,
  resumable,
  pending,
  error,
  onAdopt,
}: {
  sessionId?: string;
  resumable: boolean;
  pending: boolean;
  error: unknown;
  onAdopt: () => void;
}) {
  if (sessionId) {
    return (
      <aside
        aria-label="Live session available"
        className="flex shrink-0 flex-wrap items-center gap-2 border-b border-violet-200 bg-violet-50 px-4 py-1.5 text-[11px] text-violet-900"
      >
        <span>This session has already been imported, so it can be continued.</span>
        <Link
          href={`/sessions/${sessionId}`}
          className="rounded border border-violet-300 bg-white px-2 py-0.5 font-medium hover:bg-violet-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-700"
        >
          Open live session
        </Link>
      </aside>
    );
  }

  return (
    <aside
      aria-label="Continue this session"
      className="flex shrink-0 flex-wrap items-center gap-2 border-b border-gray-200 bg-gray-50 px-4 py-1.5 text-[11px] text-gray-700"
    >
      <span>
        {resumable
          ? "Reading history only. Continuing adopts it as a native session, preserving its own identity and lifecycle."
          : "This session recorded no working directory, so it can be read but not continued."}
      </span>
      <button
        type="button"
        onClick={onAdopt}
        disabled={!resumable || pending}
        title={
          resumable ? undefined : "The archive recorded no working directory to resume into."
        }
        className="rounded border border-gray-300 bg-white px-2 py-0.5 font-medium hover:bg-gray-100 disabled:cursor-not-allowed disabled:opacity-60 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
      >
        {pending ? "Continuing..." : "Continue this session"}
      </button>
      {error instanceof Error && (
        <span role="alert" className="text-red-700">
          {error.message}
        </span>
      )}
    </aside>
  );
}
