"use client";

/**
 * Pick a directory on the machine the backend runs on.
 *
 * This exists because a browser cannot name a local directory. There is no
 * trustworthy path in a `<input type="file">` (browsers deliberately hide it),
 * `~` cannot be expanded client-side, and the machine that matters is the one
 * running the backend — which may not be the one running the browser. So the
 * server enumerates and the client navigates, one level per request.
 *
 * Navigation is a single current directory rather than an expandable tree. A tree
 * is the right shape for the Artifacts pane, where the question is "where is this
 * file in the project"; here the question is "which directory do I want", and a
 * single level with a breadcrumb answers it in fewer clicks and no horizontal
 * scrolling. It also matches every native folder picker, which is what a user
 * arrives with expectations from.
 *
 * The browsable set is bounded by the backend's configured roots, so this
 * component never has to reason about what is off-limits — a refused path is
 * simply an error to show, and `parent` is absent at a root rather than the
 * client deciding where to stop.
 *
 * # Why the list is now a filterable combobox
 *
 * It was a plain `<ul>` of buttons: no filter, no arrow keys, no Enter. A
 * directory like `~/Workspace` holds dozens of children and the listing caps at
 * 2000, so the only way through was to read and click — the same defect the model
 * catalogue had, on the surface that backs every "where should this go" question in
 * the app. It now shares `ui/select-list` with every other choice list, which is
 * also what makes the cursor, the ARIA pattern, and scroll-into-view identical
 * here rather than a fourth hand-rolled copy.
 *
 * # Enter opens, Mod+Enter chooses
 *
 * A click has always navigated rather than chosen, deliberately: a click that
 * meant both is the ambiguity that makes double-click pickers feel unpredictable.
 * The keyboard inherits that, so Enter opens the row under the cursor. But a
 * keyboard user then had no way to *choose* without leaving the list, so the
 * modifier chooses the highlighted folder outright — the one gesture that was
 * missing to make the keyboard path complete rather than merely present.
 */

import { useCallback, useMemo, useState, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type BrowseRoot } from "@/lib/api";
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
import {
  ChevronRightIcon,
  ExternalLinkIcon,
  FolderIcon,
  FolderUpIcon,
} from "@/components/icons";

/**
 * A folder row, carrying the path the cursor needs.
 *
 * The path is on the row rather than looked up by label at commit time: two
 * entries in one directory cannot share a name, but a *filtered* list is matched
 * against by name only because that is what the row renders — and a lookup keyed on
 * rendered text is a lookup that breaks the moment the rendering changes. The
 * engine is generic over the row type precisely so a caller can carry its own
 * identity here.
 */
interface FolderChoice extends SelectChoice {
  path: string;
}

/**
 * Split a path into cumulative crumbs, stopping at the deepest root that
 * contains it.
 *
 * Crumbs below the root are not offered: they would be refused by the next
 * request, and an affordance that leads to an error is worse than its absence.
 * The *deepest* containing root matters when roots nest — `$HOME` plus
 * `$HOME/Workspace` — where the shallower one would otherwise expose crumbs the
 * user did not configure as a starting point.
 */
export function breadcrumbs(
  path: string,
  roots: BrowseRoot[],
): { name: string; path: string }[] {
  const containing = roots
    .filter((root) => path === root.path || path.startsWith(`${root.path}/`))
    .sort((a, b) => b.path.length - a.path.length)[0];
  if (!containing) return [{ name: path, path }];

  const crumbs = [{ name: containing.name, path: containing.path }];
  const rest = path.slice(containing.path.length).split("/").filter(Boolean);
  let current = containing.path;
  for (const segment of rest) {
    current = `${current}/${segment}`;
    crumbs.push({ name: segment, path: current });
  }
  return crumbs;
}

/**
 * Read a typed query as an absolute path, or `null` when it is a plain filter.
 *
 * Typing a path is the fastest route to a directory the list has never shown —
 * pasted from a terminal, or recalled in full — and it is the one input this
 * surface can validate for free, since the next listing request either resolves it
 * or reports why not.
 *
 * `~` is expanded here rather than sent, because the backend takes absolute paths
 * and a browser cannot expand it: `home` arrives with the roots listing precisely
 * so this is possible. A trailing slash is dropped so `~/Workspace/` and
 * `~/Workspace` are one path rather than two cache entries.
 */
export function pathFromQuery(query: string, home: string | undefined): string | null {
  const trimmed = query.trim();
  if (trimmed === "~") return home ?? null;
  if (trimmed.startsWith("~/")) {
    // Without `home` the expansion would be a guess, and a guessed absolute path
    // is worse than no affordance: it would offer a row leading to a refusal.
    return home ? `${home}/${stripTrailing(trimmed.slice(2))}` : null;
  }
  if (trimmed.startsWith("/")) {
    const path = stripTrailing(trimmed);
    // `/` itself is a legitimate target only if it is a configured root, and the
    // request answers that. An empty result would mean "the first root", which is
    // not what the user typed.
    return path === "" ? "/" : path;
  }
  return null;
}

function stripTrailing(path: string): string {
  return path.replace(/\/+$/, "");
}

export function DirectoryPicker({
  onClose,
  onChoose,
  title,
  /** Verb on the confirm button; the caller's action, not a generic "OK". */
  confirmLabel,
  /** Directory to open at. Falls back to the first configured root. */
  initialPath,
}: {
  onClose: () => void;
  onChoose: (path: string) => void;
  title?: string;
  confirmLabel?: string;
  initialPath?: string;
}) {
  const t = useT();
  const rootsQuery = useQuery({
    queryKey: ["fs", "roots"],
    queryFn: () => api.fs.roots(),
    staleTime: 60_000,
  });

  // `undefined` means "not chosen yet", which the tree query resolves by asking
  // for the empty path — the backend answers with its first root, so the picker
  // needs no knowledge of what the roots are before its first request.
  const [path, setPath] = useState<string | undefined>(initialPath);

  const treeQuery = useQuery({
    queryKey: ["fs", "tree", path ?? ""],
    queryFn: () => api.fs.tree(path ?? ""),
  });

  const roots = rootsQuery.data?.roots ?? [];
  const home = rootsQuery.data?.home;
  // Derived, not mirrored into state: on the first request `path` is undefined and
  // the server answers with the root it chose, so the response *is* the current
  // directory. Copying it into state through an effect would re-render for a value
  // already in hand — and would fight the user's next click for a frame.
  const current = path ?? treeQuery.data?.path;
  const crumbs = current ? breadcrumbs(current, roots) : [];
  const parent = treeQuery.data?.parent;
  const entries = treeQuery.data?.entries;

  const items = useMemo<FolderChoice[]>(() => {
    // Depends on `treeQuery.data.entries` rather than a `?? []` fallback, which
    // would allocate a fresh array each render and rebuild every row on every
    // keystroke.
    if (!entries) return [];
    return entries.map((entry) => ({
      label: entry.name,
      path: entry.path,
      icon: entry.isSymlink ? <ExternalLinkIcon size={15} /> : <FolderIcon size={15} />,
      // Navigating in, not choosing: the footer button chooses. Preserving that
      // split is what keeps one gesture from meaning two things.
      onSelect: () => setPath(entry.path),
    }));
  }, [entries]);

  // No `onClose`, for a sharper reason than in `WorkspacePicker`: committing a row
  // here *navigates into it*, so tearing down the surface on commit would close the
  // picker on the first folder the user opened. `Overlay` owns Escape.
  const list = useSelectList(items, { searchable: true });

  // A path-shaped query is a navigation target rather than a filter, so the
  // affordance is offered beside the field instead of as a row: a row would be
  // ranked against children it has nothing to do with, and dropped by the same
  // query that produced it whenever no child happened to match.
  const typedPath = pathFromQuery(list.query, home);

  const goToTypedPath = useCallback(() => {
    if (!typedPath) return;
    setPath(typedPath);
    // Cleared so the new listing is not immediately filtered by the path that
    // reached it, which would show an empty list at a directory full of children.
    list.setQuery("");
  }, [typedPath, list]);

  /**
   * Mod+Enter chooses the highlighted folder; a typed path commits on plain Enter.
   *
   * Layered over the list's own handler rather than replacing it, so arrow keys,
   * Home/End, and Escape stay in the one place that defines them. Both branches
   * run before delegation because both consume Enter, and the list would otherwise
   * treat it as "open the row under the cursor".
   */
  const onKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        const row = list.rows.find((candidate) => candidate.index === list.activeIndex);
        // Falls back to the directory being listed when the cursor is on nothing —
        // an empty or fully filtered list — which is what the confirm button would
        // do and therefore what the modifier should not refuse to do.
        const target = row?.item.path ?? current;
        if (target) onChoose(target);
        return;
      }
      if (event.key === "Enter" && typedPath) {
        event.preventDefault();
        goToTypedPath();
        return;
      }
      list.onKeyDown(event);
    },
    [list, current, onChoose, typedPath, goToTypedPath],
  );

  const error = treeQuery.error ?? rootsQuery.error;

  return (
    <Overlay
      title={title ?? t("directoryPicker.title")}
      onClose={onClose}
      describedBy="directory-picker-hint"
    >
      <div className="flex min-h-0 flex-1 flex-col md:h-[26rem]">
        {roots.length > 1 && (
          <div
            // `role="group"` because an `aria-label` on a bare `div` is dropped
            // outright: with no role there is nothing for the name to name.
            role="group"
            aria-label={t("directoryPicker.roots")}
            className="flex shrink-0 flex-wrap gap-1 border-b border-line px-3 py-2"
          >
            {roots.map((root) => (
              <button
                key={root.path}
                type="button"
                onClick={() => setPath(root.path)}
                aria-current={current === root.path ? "true" : undefined}
                className={`rounded px-2 py-1 text-xs font-medium ${
                  current === root.path
                    ? "bg-accent text-accent-ink"
                    : "text-ink-soft hover:bg-hover"
                } focus-ring`}
              >
                {root.name}
              </button>
            ))}
          </div>
        )}

        <div className="flex shrink-0 items-center gap-1 border-b border-line px-3 py-2">
          {/* Going up lives in the breadcrumb bar rather than as a row in the list.
              As a row it was both filterable — "par" matched it — and the row the
              cursor opened on, so the first Enter in a fresh directory navigated
              *away* from it. Here it sits beside the trail it belongs to, and the
              cursor starts on the first child, which is what a user means to reach. */}
          <button
            type="button"
            disabled={!parent}
            onClick={() => parent && setPath(parent)}
            aria-label={t("directoryPicker.parent")}
            title={t("directoryPicker.parent")}
            className="shrink-0 rounded p-1 text-ink-muted hover:bg-hover hover:text-ink focus-ring disabled:opacity-30 disabled:hover:bg-transparent"
          >
            <FolderUpIcon size={15} />
          </button>

          <nav
            aria-label={t("directoryPicker.path")}
            className="flex min-w-0 flex-1 flex-wrap items-center gap-0.5 text-xs"
          >
            {crumbs.map((crumb, index) => (
              <span key={crumb.path} className="flex items-center gap-0.5">
                {index > 0 && (
                  <span aria-hidden="true" className="text-ink-faint">
                    /
                  </span>
                )}
                <button
                  type="button"
                  onClick={() => setPath(crumb.path)}
                  aria-current={index === crumbs.length - 1 ? "page" : undefined}
                  className={`rounded px-1 py-0.5 ${
                    index === crumbs.length - 1
                      ? "font-semibold text-ink"
                      : "text-ink-muted hover:bg-hover hover:text-ink"
                  } focus-ring`}
                >
                  {crumb.name}
                </button>
              </span>
            ))}
          </nav>
        </div>

        {/* `onKeyDown` is re-bound so the field carries the picker's own Enter
            semantics. The filter is where focus is, so it is the only node whose
            handler can see them. */}
        <div onKeyDown={onKeyDown}>
          <SelectFilter
            list={list}
            name={t("directoryPicker.filterName")}
            placeholder={t("directoryPicker.filterPlaceholder")}
          />
        </div>

        {typedPath && (
          <button
            type="button"
            onClick={goToTypedPath}
            className="flex shrink-0 items-center gap-2 border-b border-line bg-sunken px-3 py-1.5 text-left text-xs text-ink-soft hover:bg-hover focus-ring"
          >
            <span aria-hidden className="shrink-0 text-ink-muted">
              <ChevronRightIcon size={14} />
            </span>
            <span className="min-w-0 flex-1 truncate font-mono">
              {t("directoryPicker.goTo", { path: shortenHome(typedPath, home) })}
            </span>
          </button>
        )}

        <SelectScroll list={list} label={t("directoryPicker.folders")} fill>
          {error ? (
            <div role="alert" className="px-3 py-3 text-xs">
              <p className="font-medium text-danger">{describeError(error, t)}</p>
              <button
                type="button"
                onClick={() => void treeQuery.refetch()}
                className="mt-1.5 rounded border border-danger-line bg-raised px-2 py-1 font-medium text-danger hover:bg-danger-bg focus-ring"
              >
                {t("app.retry")}
              </button>
            </div>
          ) : treeQuery.isPending ? (
            <output aria-live="polite" className="block px-3 py-3 text-xs text-ink-muted">
              {t("directoryPicker.loading")}
            </output>
          ) : items.length === 0 ? (
            // Distinct from "no match": an empty directory is still a valid answer
            // to "which folder", and saying so is what stops it reading as a dead
            // end. `SelectRows` would say "No matches", which is not the situation.
            <p className="px-3 py-3 text-xs text-ink-muted">{t("directoryPicker.empty")}</p>
          ) : (
            <>
              <SelectRows list={list} />
              {treeQuery.data?.truncated && (
                <p className="px-3 py-2 text-2xs text-ink-muted">
                  {t("directoryPicker.truncated")}
                </p>
              )}
            </>
          )}
        </SelectScroll>

        <footer className="shrink-0 border-t border-line pb-safe md:pb-0">
          <p id="directory-picker-hint" className="px-3 pt-1.5 text-3xs text-ink-muted">
            {t("directoryPicker.keys")}
          </p>
          <div className="flex items-center justify-between gap-3 px-3 pb-2 pt-1">
            <p className="min-w-0 truncate text-2xs text-ink-muted" title={current}>
              {current ? shortenHome(current, home) : ""}
            </p>
            <div className="flex shrink-0 gap-1.5">
              <button
                type="button"
                onClick={onClose}
                className="rounded-md border border-line px-3 py-1 text-xs font-medium hover:bg-sunken focus-ring"
              >
                {t("action.cancel")}
              </button>
              <button
                type="button"
                disabled={!current}
                onClick={() => current && onChoose(current)}
                className="rounded-md bg-accent px-3 py-1 text-xs font-medium text-accent-ink hover:bg-accent-hover focus-ring disabled:opacity-50"
              >
                {confirmLabel ?? t("directoryPicker.confirm")}
              </button>
            </div>
          </div>
        </footer>
      </div>
    </Overlay>
  );
}

function describeError(error: unknown, t: Translate): string {
  if (error instanceof Error) return error.message;
  return t("directoryPicker.listFailed");
}
