"use client";

/**
 * `Cmd+Shift+K`: browse one workspace's history.
 *
 * The deep-browse counterpart to the sidebar, which shows the most recent rows per
 * workspace. Scoped to a directory rather than global, which is what separates it from
 * `Cmd+K`: search answers "which session mentioned X" across everything, this answers
 * "what have I done in this project" — the question a user has when they open a
 * workspace and see nothing familiar.
 *
 * **One store, so one source.** This used to page a separate archive with a cursor and
 * fold fork lineage across page boundaries in the client, because the server held no
 * state between requests and measured fork pairs sit hundreds of rows apart. Both are gone:
 * rows are sessions, and a fork is a session of its own that belongs in the listing.
 *
 * # A known limitation, and it is the server's to fix
 *
 * `GET /sessions` takes `limit` (max 200) and **no cursor** (`docs/api-design.md` §
 * Sessions), so this surface can no longer reach past the newest 200 sessions in a
 * directory — and a real directory holds over a thousand. That is a regression against
 * the paged archive this replaces, and it is stated on screen rather than hidden,
 * because a list that silently stops at 200 reads as a directory with 200 sessions in
 * it. The filter is explicitly scoped to what loaded for the same reason: a filter that
 * searches a truncated set while looking like it searches the directory is the worse
 * failure. `Cmd+K` reaches the rest by text, which is a different question but a real
 * answer.
 */

import { useRouter } from "next/navigation";
import dynamic from "next/dynamic";
import { useCallback, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { workspacesQuery } from "@/lib/workspaces-query";
import { useT } from "@/lib/i18n";
import { formatRelative, sessionLabel, shortenHome } from "@/lib/format";
import { relayName, useEnvironments } from "@/lib/environments";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { EnvironmentChip, FidelityMarker } from "@/components/badges";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";
import { ChevronDownIcon } from "@/components/icons";

// Split out because it is reached only by opening the switcher, while this overlay
// itself is reached by a shortcut. The picker in turn lazy-loads the filesystem
// browser behind it, so neither is in this chunk.
const WorkspacePicker = dynamic(
  () => import("@/components/workspace-picker").then((m) => m.WorkspacePicker),
  { ssr: false },
);

/**
 * Rows requested, which is also the endpoint's maximum.
 *
 * Asked for in full rather than in pages because there is no cursor to continue with:
 * a smaller number would truncate more with nothing to reach the rest.
 */
const LIMIT = 200;

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

  // Shared with every other surface that lists workspaces (`lib/workspaces-query.ts`):
  // there is one workspace listing, so one key and one request.
  const workspacesResult = useQuery(workspacesQuery());

  // Falls back to the most recently used workspace when nothing set a scope —
  // opening the browser from the start screen should still show something useful
  // rather than an empty picker.
  const workspaces = workspacesResult.data?.workspaces ?? [];
  const workdir = activeWorkdir ?? workspaces[0]?.path ?? null;
  const home = workspacesResult.data?.home;

  const environments = useEnvironments();

  const history = useQuery({
    // Not `liveSessionsQuery`'s key: that one is unfiltered and polled for the
    // sidebar, and this is a directory-scoped read that must not disturb it.
    queryKey: ["sessions", { workdir, limit: LIMIT }],
    enabled: Boolean(workdir),
    queryFn: () => api.sessions.list({ workdir: workdir!, limit: LIMIT }),
  });

  // Depends on `history.data` rather than a `?? []` fallback in the memo below: the
  // fallback allocates a fresh array on every render while the query is pending, which
  // would rebuild every row on every keystroke instead of only when the listing
  // arrives. Same reason the workspace picker reads its query data directly.
  const sessions = useMemo(() => history.data?.sessions ?? [], [history.data]);
  // The endpoint caps at `LIMIT`, so a full page is indistinguishable from a directory
  // holding exactly that many — and with no cursor there is nothing to ask for next.
  // Reported as a bound rather than as a count, since that is what it is.
  const truncated = sessions.length >= LIMIT;

  const items = useMemo<SelectChoice[]>(
    () =>
      sessions.map((session) => ({
        // A string, because the label is what the filter matches. `sessionLabel` falls
        // back to `agent · leaf` for an untitled session, which is most of them for
        // `codex` and `pi` — those record no title at all — so making the fallback
        // searchable is what keeps those rows reachable by the filter.
        label: sessionLabel(session),
        detail: [session.agent, shortenHome(session.workdir, home)]
          .filter(Boolean)
          .join(" · "),
        meta: (
          <span className="flex items-center gap-1.5">
            <FidelityMarker value={session.fidelity} />
            {environments.showEnvironment && (
              <EnvironmentChip
                name={session.environmentName}
                reachableVia={relayName(session.environment, environments)}
              />
            )}
            {formatRelative(session.updatedAtMs)}
          </span>
        ),
        // One destination. A row used to open either a live session or a read-only
        // archive view depending on whether it had been imported; there is one kind of
        // session now, and every row is continuable.
        onSelect: () => {
          closeOverlay();
          router.push(`/sessions/${encodeURIComponent(session.id)}`);
        },
      })),
    [sessions, home, environments, closeOverlay, router],
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
      // having found an empty history.
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

      {/* A button into a filterable switcher, not a `<select>`. A native select of a
          real machine's workspaces is 200+ options with identical `~/Workspace/`
          prefixes, navigable only by first letter, and it could not reach a directory
          that has no sessions at all. The switcher filters by any part of the path and
          offers the filesystem as a fallback. */}
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
              : t(truncated ? "history.loadedMore" : "history.loaded", {
                  count: sessions.length,
                })}
          </span>
        </div>
      </div>

      <SelectScroll list={list} label={t("history.list")} fill>
        {history.isPending ? (
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
          {/* Stated only when it bites. The listing endpoint has no cursor, so past
              this cap there is nothing to page to — saying so is what stops a
              truncated list reading as a complete one. */}
          {truncated && ` · ${t("history.capped", { count: LIMIT })}`}
        </span>
      </footer>
    </Overlay>
  );
}
