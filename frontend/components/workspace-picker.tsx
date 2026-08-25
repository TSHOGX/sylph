"use client";

/**
 * Choose a workspace: the directories this machine has actually worked in, ranked
 * by recency, filtered by any part of the path — with the filesystem as a fallback.
 *
 * # Why this is a component of its own
 *
 * It used to be a private function inside `history-overlay.tsx`, reachable only
 * from the history browser. Meanwhile `Mod+N` and the sidebar's "New Workspace…"
 * both opened `DirectoryPicker` directly, so the two most common ways of naming a
 * directory started at `$HOME` and navigated down by mouse — even though the
 * backend already answers "which directories do you work in, most recent first"
 * and the component that renders that answer was fifty lines away.
 *
 * That is the actual defect: not that the switcher was missing a feature, but that
 * the surface which had the feature was not the surface the user reached. So this
 * moved out, and all three flows now open it.
 *
 * # Known directories first, the filesystem second
 *
 * A directory the archive has never seen is absent from this list by construction,
 * and refusing to offer it would make the picker unable to answer a question the
 * backend can. So "Browse folders…" is always present in the footer rather than as
 * a row: a row would be filtered away by the very query that failed to match
 * anything, which is exactly the moment it is needed.
 *
 * `includeArchived` is on, and the query key matches the history overlay's so the
 * two share one cache entry. It carries directories that exist only in TraceDB,
 * which is the majority of what a user wants here — a directory with a live session
 * is already one click away in the sidebar.
 */

import { useMemo, useState } from "react";
import dynamic from "next/dynamic";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { shortenHome } from "@/lib/format";
import { useT, type Translate } from "@/lib/i18n";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";

// Loaded on demand: most choices are made from the list above, and the filesystem
// browser is a fallback. The shell already pays for this chunk on other paths.
const DirectoryPicker = dynamic(
  () => import("@/components/directory-picker").then((m) => m.DirectoryPicker),
  { ssr: false },
);

export function WorkspacePicker({
  title,
  confirmLabel,
  current,
  onChoose,
  onClose,
  onBack,
}: {
  title: string;
  /**
   * Verb for the filesystem fallback's confirm button.
   *
   * The caller's own action, because it differs per flow — start a session, bind a
   * workspace, show its history — and a generic "OK" on the last step of a
   * three-surface path is where a user loses track of what they are confirming.
   */
  confirmLabel: string;
  /** Marks the row that is already in effect. Absent when there is no current. */
  current?: string | null;
  onChoose: (path: string) => void;
  onClose: () => void;
  /** Returns to the surface this replaced, when it replaced one. */
  onBack?: () => void;
}) {
  const t = useT();
  const [browsing, setBrowsing] = useState(false);

  const workspacesQuery = useQuery({
    // Shared with the history overlay deliberately: same request, same shape, and
    // one of the two surfaces having a colder cache than the other is a difference
    // the user would feel as an inconsistent wait.
    queryKey: ["workspaces", { includeArchived: true }],
    queryFn: () => api.workspaces.list({ includeArchived: true }),
    staleTime: 60_000,
  });

  const workspaces = workspacesQuery.data?.workspaces;
  const home = workspacesQuery.data?.home;

  const items = useMemo<SelectChoice[]>(() => {
    // Depends on `workspacesQuery.data` rather than a `?? []` fallback, which
    // would allocate a fresh array while the query is pending and rebuild every
    // row on every keystroke.
    if (!workspaces) return [];
    return [...workspaces]
      .sort((a, b) => b.lastUsedMs - a.lastUsedMs)
      .map((workspace) => ({
        label: workspace.name,
        // The path is the searchable line, which is the whole reason `detail`
        // exists in the row model: a project is remembered by a middle segment
        // that its leaf name does not carry.
        detail: shortenHome(workspace.path, home),
        meta: describeCounts(workspace.archivedCount, workspace.sessionCount, t),
        // `undefined` rather than `false` when the caller named no current
        // directory, so the check column is not reserved on a list where no row
        // could ever carry one — and so a row is announced as an action rather
        // than as an unchecked option.
        checked: current == null ? undefined : workspace.path === current,
        onSelect: () => onChoose(workspace.path),
      }));
  }, [workspaces, home, current, onChoose, t]);

  // Always filterable, never `auto`: the threshold heuristic counts single-select
  // rows, and a new-session list marks none of them current. The list is also
  // long on any real machine — a measured local archive holds directories in the
  // hundreds — so the answer is known in advance and does not need deriving.
  //
  // Deliberately no `onClose`. This list does not own the surface: what happens
  // after a choice is the *caller's* decision — `Mod+N` closes and creates, while
  // the history browser swaps back to its session list — and a list that also tore
  // the surface down would make the second impossible. Escape then bubbles to
  // `Overlay`, which owns dismissal; claiming it here with nothing to close would
  // swallow the key outright (WCAG 2.1.2).
  const list = useSelectList(items, { searchable: true });

  // Replaces this surface rather than stacking on it, so only one focus trap is
  // ever mounted. Cancelling returns here, which is where the user came from.
  if (browsing) {
    return (
      <DirectoryPicker
        title={t("workspacePicker.browseTitle")}
        confirmLabel={confirmLabel}
        initialPath={current ?? undefined}
        onChoose={onChoose}
        onClose={() => setBrowsing(false)}
      />
    );
  }

  return (
    <Overlay title={title} onClose={onClose} describedBy="workspace-picker-hint">
      <SelectFilter
        list={list}
        name={t("workspacePicker.filterName")}
        placeholder={t("workspacePicker.filterPlaceholder")}
      />

      <SelectScroll list={list} label={title} fill>
        {workspacesQuery.isPending ? (
          <output aria-live="polite" className="block px-3 py-6 text-center text-xs text-ink-muted">
            {t("workspacePicker.loading")}
          </output>
        ) : workspacesQuery.error ? (
          <div role="alert" className="px-3 py-6 text-center text-xs">
            <p className="font-medium text-danger">{t("workspacePicker.loadFailed")}</p>
            <button
              type="button"
              onClick={() => void workspacesQuery.refetch()}
              className="mt-1.5 rounded border border-danger-line px-2 py-1 font-medium text-danger hover:bg-danger-bg focus-ring"
            >
              {t("app.retry")}
            </button>
          </div>
        ) : items.length === 0 ? (
          // Distinct from "no match": an empty archive is answered by browsing,
          // and saying so is the difference between a dead end and a next step.
          <p className="px-3 py-6 text-center text-xs text-ink-muted">
            {t("workspacePicker.empty")}
          </p>
        ) : (
          <SelectRows list={list} />
        )}
      </SelectScroll>

      <footer
        id="workspace-picker-hint"
        className="flex shrink-0 items-center justify-between gap-2 border-t border-line bg-sunken px-3 py-1.5 pb-safe md:pb-1.5"
      >
        <span className="text-3xs text-ink-muted">{t("select.keys")}</span>
        <div className="flex shrink-0 gap-1.5">
          {onBack && (
            <button
              type="button"
              onClick={onBack}
              className="rounded border border-line bg-raised px-2 py-0.5 text-3xs font-medium hover:bg-sunken focus-ring"
            >
              {t("app.back")}
            </button>
          )}
          <button
            type="button"
            onClick={() => setBrowsing(true)}
            className="rounded border border-line bg-raised px-2 py-0.5 text-3xs font-medium hover:bg-sunken focus-ring"
          >
            {t("workspacePicker.browse")}
          </button>
        </div>
      </footer>
    </Overlay>
  );
}

/**
 * What a directory holds, or nothing at all.
 *
 * Shown because it is the difference between a directory worth opening and one
 * that renders an empty list. Returns `undefined` rather than an empty string when
 * there is nothing to report, so `meta` stays absent and the row does not reserve
 * a column for a value it never has.
 */
function describeCounts(archived: number, live: number, t: Translate): string | undefined {
  const parts: string[] = [];
  if (archived > 0) parts.push(t("workspacePicker.archived", { count: archived }));
  if (live > 0) parts.push(t("workspacePicker.live", { count: live }));
  return parts.length > 0 ? parts.join(" · ") : undefined;
}
