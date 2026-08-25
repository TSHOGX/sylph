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
import { ApiError, api, type ArchiveDelegate } from "@/lib/api";
import { formatTimestamp } from "@/lib/format";
import { densityClasses, useSettings } from "@/lib/settings";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Timeline } from "@/components/timeline";
import { ContextMenu, type MenuItem } from "@/components/ui/menu";

/** Events fetched at once. The largest archived session here is 8167 events. */
const EVENT_LIMIT = 2000;

export default function ArchivedSessionPage() {
  const params = useParams();
  const router = useRouter();
  const queryClient = useQueryClient();
  const { settings } = useSettings();
  const { showArtifact, setActiveWorkdir } = useWorkspaceUI();
  const [expandAll, setExpandAll] = useState<boolean | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

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
        className="flex flex-1 items-center justify-center text-sm text-ink-muted"
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
          <h1 className="text-lg font-semibold text-danger">
            {missing
              ? "Not in the archive"
              : unavailable
                ? "Archive unavailable"
                : "Could not load this session"}
          </h1>
          <p className="mt-2 text-sm text-ink-muted">
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
              className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-sunken focus-ring"
            >
              Retry
            </button>
            <Link
              href="/"
              className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-ink hover:bg-accent-hover focus-ring"
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
  const menuItems: MenuItem[] = [
    {
      label: "Copy original session ID",
      onSelect: () => void navigator.clipboard.writeText(data.id).catch(() => {}),
    },
    ...(data.cwd
      ? [{
          label: "Copy workspace path",
          onSelect: () => void navigator.clipboard.writeText(data.cwd!).catch(() => {}),
        }]
      : []),
    {
      label: "Refresh transcript",
      onSelect: () => void transcript.refetch(),
    },
  ];

  return (
    <>
      <header
        className="shrink-0 border-b border-line px-4 py-2"
        onContextMenu={(event) => {
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
      >
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="truncate text-sm font-semibold">{title}</h1>
          <span className="shrink-0 rounded bg-hover px-1.5 py-0.5 text-3xs font-medium text-ink-soft">
            archived · read-only
          </span>
          <div className="ml-auto flex items-center gap-1">
            <button
              type="button"
              onClick={() => setExpandAll(expandAll === true ? null : true)}
              className="rounded px-1.5 py-0.5 text-3xs text-ink-muted hover:bg-hover hover:text-ink focus-ring"
            >
              Expand all
            </button>
            <button
              type="button"
              onClick={() => setExpandAll(expandAll === false ? null : false)}
              className="rounded px-1.5 py-0.5 text-3xs text-ink-muted hover:bg-hover hover:text-ink focus-ring"
            >
              Collapse all
            </button>
          </div>
        </div>
        <p className="mt-0.5 truncate text-2xs text-ink-muted">
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
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems}
          onClose={() => setMenu(null)}
          label={`${title} actions`}
        />
      )}

      <LineageBar parentId={data.parentId} forkedFrom={data.forkedFrom} />

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
          className="shrink-0 border-b border-warning-line bg-warning-bg px-4 py-1.5 text-2xs text-warning"
        >
          Showing the first {EVENT_LIMIT.toLocaleString()} events of a longer session.
        </aside>
      )}

      <div className="min-h-0 flex-1 pane px-4 py-3">
        <div className={`mx-auto max-w-3xl ${density.text}`}>
          <Delegations
            delegations={data.delegations ?? []}
            truncated={data.delegationsTruncated}
          />
          {data.events.length === 0 ? (
            <p className="py-8 text-center text-sm text-ink-muted">
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
 * Where this session sits in the archive's session-level tree.
 *
 * Half of a real archive is delegated work, so a transcript that says nothing
 * about its lineage misrepresents most of what it renders: a delegate opened from
 * a search hit reads as a conversation someone started. Rendered as a link rather
 * than a label because the parent is the context that explains the transcript.
 *
 * A fork is kept visually distinct and separately worded. It is a different
 * relationship — the same conversation resumed, replaying its origin's whole
 * prefix — and calling it a parent would claim delegation that did not happen.
 */
function LineageBar({ parentId, forkedFrom }: { parentId?: string; forkedFrom?: string }) {
  if (!parentId && !forkedFrom) return null;

  return (
    <aside
      aria-label="Session lineage"
      className="flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-line bg-sunken px-4 py-1.5 text-2xs text-ink-soft"
    >
      {parentId && (
        <>
          <span>Delegated work. Its parent session holds the request that started it.</span>
          <Link
            href={`/archive/${encodeURIComponent(parentId)}`}
            className="rounded border border-line-strong bg-raised px-2 py-0.5 font-medium hover:bg-hover focus-ring"
          >
            Open parent session
          </Link>
        </>
      )}
      {forkedFrom && (
        <>
          <span>
            Forked from another session, so it replays that conversation before continuing.
          </span>
          <Link
            href={`/archive/${encodeURIComponent(forkedFrom)}`}
            className="rounded border border-line-strong bg-raised px-2 py-0.5 font-medium hover:bg-hover focus-ring"
          >
            Open origin session
          </Link>
        </>
      )}
    </aside>
  );
}

/**
 * The sessions this one delegated to.
 *
 * Above the transcript rather than inside it, and that placement is the honest
 * part. A live turn carries delegation per event, so the timeline can nest a
 * subagent's work inside the tool call that spawned it. The archive stores no
 * equivalent: measured across all five agents, no archived event carries a parent
 * pointer of that kind, and the id suffix that might attribute a child to a
 * specific call is an opaque hash in 1918 of 2104 cases. So there is no honest
 * place *inside* the transcript to put these, and putting them at a guessed one
 * would render nine in ten delegations under the wrong tool call while looking
 * exactly like the live tree.
 *
 * What is real is that these sessions were delegated by this one, and that each is
 * a transcript of its own. That is what this renders: a list, at the top, that
 * says its own limits.
 */
function Delegations({
  delegations,
  truncated,
}: {
  delegations: ArchiveDelegate[];
  truncated: boolean;
}) {
  if (delegations.length === 0) return null;

  return (
    <section aria-labelledby="delegations-heading" className="mb-4 rounded-lg border border-line">
      <h2
        id="delegations-heading"
        className="border-b border-line bg-sunken px-3 py-1.5 text-2xs font-medium text-ink-soft"
      >
        Delegated {delegations.length} session{delegations.length === 1 ? "" : "s"}
        {truncated && " (partial list)"}
      </h2>
      <ul className="divide-y divide-line">
        {delegations.map((delegate) => (
          <li key={delegate.id}>
            <Link
              href={`/archive/${encodeURIComponent(delegate.id)}`}
              className="block px-3 py-1.5 hover:bg-hover focus-ring"
            >
              <span className="flex items-baseline gap-2">
                <span className="min-w-0 flex-1 truncate text-xs text-ink">
                  {delegate.title ?? (
                    <span className="text-ink-muted">Untitled {delegate.agent} session</span>
                  )}
                </span>
                <span className="shrink-0 text-3xs text-ink-muted">
                  {delegate.events.toLocaleString()} events
                </span>
              </span>
              {/* Only present when it differs from this session's own directory,
                  which is the case worth showing: a delegate commonly runs in a
                  worktree, and 484 of 2238 measured did. */}
              {delegate.cwd && (
                <span className="mt-0.5 block truncate font-mono text-3xs text-ink-muted">
                  ran in {delegate.cwd}
                </span>
              )}
            </Link>
          </li>
        ))}
      </ul>
      <p className="border-t border-line px-3 py-1 text-3xs text-ink-muted">
        {/* Said plainly, because the alternative is a reader inferring that the
            transcript below is missing structure it never had. */}
        Each delegation is a session of its own. The archive records no link to the
        exact tool call that started one, so they are listed here rather than nested
        in the transcript.
      </p>
    </section>
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
        className="flex shrink-0 flex-wrap items-center gap-2 border-b border-native-line bg-native-bg px-4 py-1.5 text-2xs text-native"
      >
        <span>This session has already been imported, so it can be continued.</span>
        <Link
          href={`/sessions/${sessionId}`}
          className="rounded border border-native-line bg-raised px-2 py-0.5 font-medium hover:bg-native-bg focus-ring"
        >
          Open live session
        </Link>
      </aside>
    );
  }

  return (
    <aside
      aria-label="Continue this session"
      className="flex shrink-0 flex-wrap items-center gap-2 border-b border-line bg-sunken px-4 py-1.5 text-2xs text-ink-soft"
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
        className="rounded border border-line-strong bg-raised px-2 py-0.5 font-medium hover:bg-hover disabled:cursor-not-allowed disabled:opacity-60 focus-ring"
      >
        {pending ? "Continuing..." : "Continue this session"}
      </button>
      {error instanceof Error && (
        <span role="alert" className="text-danger">
          {error.message}
        </span>
      )}
    </aside>
  );
}
