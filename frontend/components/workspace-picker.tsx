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
 * A directory nothing has ever run in is absent from this list by construction, and
 * refusing to offer it would make the picker unable to answer a question the backend
 * can. So "Browse folders…" is always present in the footer rather than as a row: a
 * row would be filtered away by the very query that failed to match anything, which
 * is exactly the moment it is needed.
 *
 * The listing comes from `lib/workspaces-query.ts`, shared with every other surface that
 * lists workspaces: one store, one listing, one request.
 */

import { useMemo, useState } from "react";
import dynamic from "next/dynamic";
import { useQuery } from "@tanstack/react-query";
import { workspacesQuery } from "@/lib/workspaces-query";
import { shortenHome } from "@/lib/format";
import { useT, type Translate } from "@/lib/i18n";
import { defaultEnvironment, useEnvironments } from "@/lib/environments";
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
  environment: requestedEnvironment,
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
  current?: { path: string; environment: string | null } | null;
  onChoose: (path: string, environment?: string | null) => void;
  onClose: () => void;
  /** Returns to the surface this replaced, when it replaced one. */
  onBack?: () => void;
  environment?: string | null;
}) {
  const t = useT();
  const environments = useEnvironments();
  const browseEnvironment = current?.environment ?? requestedEnvironment ?? defaultEnvironment(environments.all)?.id ?? null;
  const [browsing, setBrowsing] = useState(false);

  // Shared deliberately (`lib/workspaces-query.ts`): one surface having a colder cache
  // than another is a difference the user feels as an inconsistent wait.
  const workspacesResult = useQuery(workspacesQuery());

  const workspaces = workspacesResult.data?.workspaces;

  const items = useMemo<SelectChoice[]>(() => {
    // Depends on `workspacesResult.data` rather than a `?? []` fallback, which
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
        detail: [
          workspace.path === "__sylph_quick_start__"
            ? "Quick Start"
            : shortenHome(workspace.path, workspace.home),
          environments.showEnvironment && workspace.environment
            ? workspace.environmentName ?? environments.nameOf(workspace.environment) ?? workspace.environment
            : undefined,
        ]
          .filter(Boolean)
          .join(" · "),
        meta: describeCount(workspace.sessionCount, t),
        // `undefined` rather than `false` when the caller named no current
        // directory, so the check column is not reserved on a list where no row
        // could ever carry one — and so a row is announced as an action rather
        // than as an unchecked option.
        checked: current == null ? undefined : workspace.path === current.path && (workspace.environment ?? null) === current.environment,
        onSelect: () => onChoose(workspace.path, workspace.environment ?? null),
      }));
  }, [workspaces, current, onChoose, t, environments]);

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
        initialPath={current?.path === "__sylph_quick_start__" ? undefined : current?.path ?? undefined}
        environment={browseEnvironment ?? undefined}
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
        {workspacesResult.isPending ? (
          <output aria-live="polite" className="block px-3 py-6 text-center text-xs text-ink-muted">
            {t("workspacePicker.loading")}
          </output>
        ) : workspacesResult.error ? (
          <div role="alert" className="px-3 py-6 text-center text-xs">
            <p className="font-medium text-danger">{t("workspacePicker.loadFailed")}</p>
            <button
              type="button"
              onClick={() => void workspacesResult.refetch()}
              className="mt-1.5 rounded border border-danger-line px-2 py-1 font-medium text-danger hover:bg-danger-bg focus-ring"
            >
              {t("app.retry")}
            </button>
          </div>
        ) : items.length === 0 ? (
          // Distinct from "no match": a machine with no recorded directories is
          // answered by browsing, and saying so is the difference between a dead end
          // and a next step.
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
 * Shown because it is the difference between a directory worth opening and one that
 * renders an empty list. Returns `undefined` rather than an empty string when there is
 * nothing to report, so `meta` stays absent and the row does not reserve a column for a
 * value it never has.
 *
 * One number now, not two. It used to report archived and live counts separately,
 * because they came from two stores and a directory could have history in one and
 * nothing in the other — which also meant neither number alone answered "is there
 * anything here". One store makes it a count.
 */
function describeCount(sessions: number, t: Translate): string | undefined {
  return sessions > 0 ? t("workspacePicker.sessions", { count: sessions }) : undefined;
}
