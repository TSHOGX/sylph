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
 */

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type BrowseRoot } from "@/lib/api";
import { Overlay } from "@/components/overlay";

/** Replace the home prefix with `~` so paths stay readable in a narrow modal. */
export function abbreviate(path: string, home: string | undefined): string {
  if (!home || !path.startsWith(home)) return path;
  const rest = path.slice(home.length);
  return rest === "" ? "~" : rest.startsWith("/") ? `~${rest}` : path;
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

export function DirectoryPicker({
  onClose,
  onChoose,
  title = "Choose a folder",
  /** Verb on the confirm button; the caller's action, not a generic "OK". */
  confirmLabel = "Use this folder",
  /** Directory to open at. Falls back to the first configured root. */
  initialPath,
}: {
  onClose: () => void;
  onChoose: (path: string) => void;
  title?: string;
  confirmLabel?: string;
  initialPath?: string;
}) {
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

  return (
    <Overlay title={title} onClose={onClose}>
      <div className="flex h-[26rem] flex-col">
        {roots.length > 1 && (
          <div className="flex shrink-0 flex-wrap gap-1 border-b border-gray-200 px-3 py-2">
            {roots.map((root) => (
              <button
                key={root.path}
                type="button"
                onClick={() => setPath(root.path)}
                className={`rounded px-2 py-1 text-xs font-medium ${
                  current === root.path
                    ? "bg-gray-900 text-white"
                    : "text-gray-700 hover:bg-gray-100"
                } focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900`}
              >
                {root.name}
              </button>
            ))}
          </div>
        )}

        <nav
          aria-label="Path"
          className="flex shrink-0 flex-wrap items-center gap-0.5 border-b border-gray-200 px-3 py-2 text-xs"
        >
          {crumbs.map((crumb, index) => (
            <span key={crumb.path} className="flex items-center gap-0.5">
              {index > 0 && (
                <span aria-hidden="true" className="text-gray-400">
                  /
                </span>
              )}
              <button
                type="button"
                onClick={() => setPath(crumb.path)}
                aria-current={index === crumbs.length - 1 ? "page" : undefined}
                className={`rounded px-1 py-0.5 ${
                  index === crumbs.length - 1
                    ? "font-semibold text-gray-900"
                    : "text-gray-600 hover:bg-gray-100 hover:text-gray-900"
                } focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900`}
              >
                {crumb.name}
              </button>
            </span>
          ))}
        </nav>

        <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1.5">
          {rootsQuery.error || treeQuery.error ? (
            <div role="alert" className="px-2 py-3 text-xs">
              <p className="font-medium text-red-800">
                {describeError(treeQuery.error ?? rootsQuery.error)}
              </p>
              <button
                type="button"
                onClick={() => void treeQuery.refetch()}
                className="mt-1.5 rounded border border-red-300 bg-white px-2 py-1 font-medium text-red-800 hover:bg-red-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600"
              >
                Retry
              </button>
            </div>
          ) : treeQuery.isPending ? (
            <output aria-live="polite" className="block px-2 py-3 text-xs text-gray-500">
              Loading folders...
            </output>
          ) : (
            <ul aria-label="Folders">
              {treeQuery.data?.parent && (
                <li>
                  <button
                    type="button"
                    onClick={() => setPath(treeQuery.data!.parent!)}
                    className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs text-gray-700 hover:bg-gray-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                  >
                    <span aria-hidden="true" className="w-4 text-gray-500">
                      ↑
                    </span>
                    <span className="text-gray-600">Parent folder</span>
                  </button>
                </li>
              )}

              {treeQuery.data?.entries.length === 0 ? (
                <li className="px-2 py-3 text-xs text-gray-500">
                  No subfolders here. Choose this folder, or go up.
                </li>
              ) : (
                treeQuery.data?.entries.map((entry) => (
                  <li key={entry.path}>
                    <button
                      type="button"
                      // Single click navigates. There is a separate confirm
                      // button, so a click never has to mean both "open" and
                      // "choose" — the ambiguity that makes double-click pickers
                      // feel unpredictable.
                      onClick={() => setPath(entry.path)}
                      onDoubleClick={() => onChoose(entry.path)}
                      className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-gray-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                    >
                      <span aria-hidden="true" className="w-4 shrink-0 text-gray-500">
                        {entry.isSymlink ? "↪" : "▸"}
                      </span>
                      <span className="truncate text-gray-900">{entry.name}</span>
                    </button>
                  </li>
                ))
              )}

              {treeQuery.data?.truncated && (
                <li className="px-2 py-2 text-[11px] text-gray-500">
                  Only the first folders are listed. Narrow down by opening one.
                </li>
              )}
            </ul>
          )}
        </div>

        <div className="flex shrink-0 items-center justify-between gap-3 border-t border-gray-200 px-3 py-2">
          <p className="min-w-0 truncate text-[11px] text-gray-600" title={current}>
            {current ? abbreviate(current, home) : ""}
          </p>
          <div className="flex shrink-0 gap-1.5">
            <button
              type="button"
              onClick={onClose}
              className="rounded-md border border-gray-200 px-3 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
            >
              Cancel
            </button>
            <button
              type="button"
              disabled={!current}
              onClick={() => current && onChoose(current)}
              className="rounded-md bg-gray-900 px-3 py-1 text-xs font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
            >
              {confirmLabel}
            </button>
          </div>
        </div>
      </div>
    </Overlay>
  );
}

function describeError(error: unknown): string {
  if (error instanceof Error) return error.message;
  return "Could not list that folder.";
}
