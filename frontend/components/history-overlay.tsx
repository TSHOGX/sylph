"use client";

/**
 * `Cmd+Shift+K`: browse one workspace's archived history.
 *
 * This is the deep-paging counterpart to the sidebar, which shows only the most
 * recent few sessions per workspace. A directory in this archive can hold over a
 * thousand sessions, so the sidebar cannot be the browse surface — a 240px pane
 * paging a thousand rows is not navigation.
 *
 * Scoped to a directory rather than global, which is what separates it from
 * `Cmd+K`. Search answers "which session mentioned X" across everything; this
 * answers "what have I done in this project", which is the question a user has
 * when they open a workspace and see nothing familiar.
 *
 * Filtering is client-side over the loaded pages. That is deliberate: TraceDB's
 * full-text search is already `Cmd+K`, and re-querying it per keystroke here
 * would spawn a subprocess per character to answer a narrower question than the
 * one the user is asking. Rows the filter cannot see are reached by paging, which
 * the footer states.
 */

import { useRouter } from "next/navigation";
import dynamic from "next/dynamic";
import { useCallback, useMemo, useState } from "react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useT } from "@/lib/i18n";
import { formatRelative, shortenHome } from "@/lib/format";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";
import { ArchiveIcon, ChevronDownIcon, OriginNativeIcon } from "@/components/icons";

// Split out because it is reached only by opening the switcher, while this overlay
// itself is reached by a shortcut. The picker in turn lazy-loads the filesystem
// browser behind it, so neither is in this chunk.
const WorkspacePicker = dynamic(
  () => import("@/components/workspace-picker").then((m) => m.WorkspacePicker),
  { ssr: false },
);

/** Rows per request. Enough to fill the list without a long first wait. */
const PAGE_SIZE = 25;

export function HistoryOverlay() {
  const t = useT();
  const router = useRouter();
  const { closeOverlay, activeWorkdir, setActiveWorkdir } = useWorkspaceUI();

  /**
   * Which surface is up. Three modes rather than three stacked modals: nesting
   * `Overlay` inside `Overlay` puts two focus traps on one DOM path, where the
   * outer trap's `querySelectorAll` also matches the inner dialog's controls and
   * Tab at the boundary becomes unpredictable. Swapping the content keeps exactly
   * one trap alive, and "back" is a state change rather than an unmount.
   */
  const [mode, setMode] = useState<"list" | "switch">("list");

  const workspacesQuery = useQuery({
    // Distinct key from the sidebar's: this variant carries archive-only
    // directories, and sharing a key would leak them into the sidebar's poll (or
    // strip them from here, depending on which query resolved last).
    queryKey: ["workspaces", { includeArchived: true }],
    queryFn: () => api.workspaces.list({ includeArchived: true }),
    staleTime: 60_000,
  });

  // Falls back to the most recently used workspace when nothing set a scope —
  // opening the browser from the home page should still show something useful
  // rather than an empty picker.
  const workspaces = workspacesQuery.data?.workspaces ?? [];
  const workdir = activeWorkdir ?? workspaces[0]?.path ?? null;
  const home = workspacesQuery.data?.home;

  const history = useInfiniteQuery({
    queryKey: ["archive", "sessions", workdir],
    enabled: Boolean(workdir),
    queryFn: ({ pageParam }) =>
      api.archive.list({ workdir: workdir!, limit: PAGE_SIZE, cursor: pageParam }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.nextCursor,
  });

  // Depends on `history.data` rather than a `?? []` fallback: the fallback
  // allocates a fresh array every render, which would make the memo below
  // recompute on every keystroke instead of only when a page arrives.
  const pages = history.data?.pages;
  const available = pages === undefined || pages.length === 0 || pages[0].available;
  const sessions = useMemo(() => {
    const all = (pages ?? []).flatMap((page) => page.sessions);

    // Collapse forks across page boundaries. A fork and its origin are one
    // conversation — both replay the same prefix — and the server drops the loser
    // within a single response, but it holds no state between requests. One
    // measured pair sits 268 rows apart, so only a client accumulating pages sees
    // both.
    //
    // Both directions matter, because either side can come first. Rows arrive
    // newest-first, so the survivor is whichever appears earlier: a fork naming an
    // origin already shown is the duplicate, and an origin whose fork was shown
    // earlier is likewise the duplicate.
    //
    // Two index maps rather than a nested scan, so this stays linear as pages
    // accumulate — a directory here can reach several hundred loaded rows.
    const firstIndexById = new Map<string, number>();
    const forkIndexByOrigin = new Map<string, number>();
    all.forEach((session, index) => {
      if (!firstIndexById.has(session.id)) firstIndexById.set(session.id, index);
      if (session.forkOf && !forkIndexByOrigin.has(session.forkOf)) {
        forkIndexByOrigin.set(session.forkOf, index);
      }
    });

    return all.filter((session, index) => {
      const originIndex = session.forkOf ? firstIndexById.get(session.forkOf) : undefined;
      // A fork whose origin appears earlier: the origin survives.
      if (originIndex !== undefined && originIndex < index) return false;
      // An origin whose fork appears earlier: the fork survives.
      const forkIndex = forkIndexByOrigin.get(session.id);
      if (forkIndex !== undefined && forkIndex < index) return false;
      return true;
    });
  }, [pages]);

  const items = useMemo<SelectChoice[]>(
    () =>
      sessions.map((session) => ({
        // A string, because the label is what the filter matches. The untitled case
        // used to be dimmed markup; `claude` records a title for most sessions
        // while `codex` and `pi` record none, so this row is common enough that
        // "Untitled <agent> session" being searchable is the better trade.
        label: session.label ?? `Untitled ${session.agent} session`,
        // Shape plus title, not colour alone (WCAG 1.4.1). The archive box marks
        // archived history throughout the app; an imported one carries the native
        // origin diamond, because that is what it becomes once imported.
        icon: (
          <span
            title={session.sessionId ? "imported session" : "archived session"}
            className={session.sessionId ? "text-native" : "text-ink-faint"}
          >
            {session.sessionId ? <OriginNativeIcon size={9} /> : <ArchiveIcon size={12} />}
          </span>
        ),
        // Agent and model on the searchable line, which is what the hand-rolled filter
        // matched: they are how a session is recalled when its title is absent.
        detail: [
          session.agent,
          `${session.events} events`,
          session.subagentCount > 0
            ? `${session.subagentCount} subagent${session.subagentCount === 1 ? "" : "s"}`
            : undefined,
          session.model ?? undefined,
        ]
          .filter(Boolean)
          .join(" · "),
        meta: (
          <span className="flex items-center gap-1.5">
            {session.sessionId && (
              <span className="rounded bg-native-bg px-1 font-medium text-native">imported</span>
            )}
            {formatRelative(session.endedAtMs ?? session.startedAtMs)}
          </span>
        ),
        onSelect: () => {
          closeOverlay();
          // An imported session has a live conversation, which is the better
          // destination: it can be continued, and the read-only view cannot.
          router.push(
            session.sessionId
              ? `/sessions/${session.sessionId}`
              : `/archive/${encodeURIComponent(session.id)}`,
          );
        },
      })),
    [sessions, closeOverlay, router],
  );

  // `searchable: true`, never `auto`: nothing here is a single-select choice, so the
  // threshold heuristic would answer "no filter" for a list that can hold hundreds of
  // rows. No `onClose` — a commit navigates, and `Overlay` owns Escape.
  const list = useSelectList(items, { searchable: true });

  const setQuery = list.setQuery;
  const chooseWorkspace = useCallback(
    (path: string) => {
      setActiveWorkdir(path);
      // The filter is cleared with the scope it was typed against. A query that
      // matched nothing in the new directory would otherwise present the switch as
      // having found an empty archive.
      setQuery("");
      setMode("list");
    },
    [setActiveWorkdir, setQuery],
  );

  if (mode === "switch") {
    return (
      <WorkspacePicker
        title={t("workspacePicker.title")}
        confirmLabel={t("history.showHistory")}
        current={workdir}
        onChoose={chooseWorkspace}
        onBack={() => setMode("list")}
        onClose={closeOverlay}
      />
    );
  }

  return (
    <Overlay title={t("history.title")} onClose={closeOverlay} describedBy="history-hint">
      {/* The filter comes first in DOM order deliberately, and it focuses itself: a
          control placed above it would otherwise be the first tabbable descendant,
          and the user's first keystroke would change the workspace instead of
          filtering. It is also what they want to type into, which is why the two
          agree. */}
      <SelectFilter
        list={list}
        name={t("history.list")}
        label={t("history.field")}
        placeholder={t("history.placeholder")}
        size="roomy"
      />

      {/* A button into a filterable switcher, not a `<select>`. A native select
          of this archive is 200+ options with identical `~/Workspace/` prefixes,
          navigable only by first letter, and it could not reach a directory that
          has no archived sessions at all. The switcher filters by any part of the
          path and offers the filesystem as a fallback. */}
      <div className="shrink-0 border-b border-line">
        <div className="flex items-center gap-2 px-3 py-2">
          <button
            type="button"
            onClick={() => setMode("switch")}
            aria-label={t("history.workspaceButton", {
              path: workdir ? shortenHome(workdir, home) : t("history.noWorkspace"),
            })}
            className="flex min-w-0 flex-1 items-center gap-1 rounded border border-line bg-raised px-1.5 py-1 text-left text-xs hover:bg-sunken focus-ring"
          >
            <span className="min-w-0 flex-1 truncate" title={workdir ?? undefined}>
              {workdir ? shortenHome(workdir, home) : t("history.chooseWorkspace")}
            </span>
            <span className="shrink-0 text-ink-muted">
              <ChevronDownIcon size={12} />
            </span>
          </button>
          <span className="shrink-0 text-3xs text-ink-muted">
            {history.isFetching && sessions.length === 0
              ? t("action.loading")
              : t(history.hasNextPage ? "history.loadedMore" : "history.loaded", {
                  count: sessions.length,
                })}
          </span>
        </div>
      </div>

      <SelectScroll list={list} label={t("history.list")} fill>
        {!available ? (
          <p className="px-3 py-6 text-center text-xs text-ink-muted">
            {t("history.archiveUnavailable")}
          </p>
        ) : history.isPending ? (
          <output aria-live="polite" className="block px-3 py-6 text-center text-xs text-ink-muted">
            {t("action.loading")}
          </output>
        ) : history.error ? (
          <div role="alert" className="px-3 py-6 text-center text-xs">
            <p className="font-medium text-danger">{t("history.loadFailed")}</p>
            <button
              type="button"
              onClick={() => void history.refetch()}
              className="mt-1.5 rounded border border-danger-line px-2 py-1 font-medium text-danger hover:bg-danger-bg focus-ring"
            >
              {t("app.retry")}
            </button>
          </div>
        ) : list.rows.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-ink-muted">
            {list.query.trim() ? t("history.none") : t("history.empty")}
          </p>
        ) : (
          <SelectRows list={list} />
        )}
      </SelectScroll>

      <footer
        id="history-hint"
        className="flex shrink-0 items-center justify-between gap-2 border-t border-line bg-sunken px-3 py-1.5 pb-safe md:pb-1.5"
      >
        <span className="text-3xs text-ink-muted">
          {t("history.keys")}
          {history.hasNextPage && ` · ${t("history.filterScope")}`}
        </span>
        {/* Paging moved out of the list. A `listbox` may only contain options, so a
            button among the rows was invalid ARIA — and it was also below however many
            rows had loaded, which is the one place a user paging a thousand sessions
            has to scroll to reach. */}
        {history.hasNextPage && (
          <button
            type="button"
            onClick={() => void history.fetchNextPage()}
            disabled={history.isFetchingNextPage}
            className="shrink-0 rounded border border-line bg-raised px-2 py-0.5 text-3xs font-medium hover:bg-sunken disabled:opacity-60 focus-ring"
          >
            {history.isFetchingNextPage ? t("action.loading") : t("history.loadOlder")}
          </button>
        )}
      </footer>
    </Overlay>
  );
}
