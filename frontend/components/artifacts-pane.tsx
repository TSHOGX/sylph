"use client";

/**
 * Right pane: the session's working directory, and a viewer for one file.
 *
 * The tree loads one directory per expansion rather than walking the whole tree
 * up front. A repository with 100k files would otherwise spend seconds building
 * a payload whose first screen is all anyone reads, and the pane would be blank
 * until it finished.
 *
 * # Tree and viewer swap, they do not share
 *
 * Locating a file and reading it are sequential, not simultaneous: you navigate,
 * then you read. So the viewer takes the whole pane with a back affordance, the
 * way VS Code's explorer hands off to an editor and Finder hands off to Quick
 * Look. The earlier layout gave the preview the bottom 55% of a 320px pane —
 * roughly 350x300px — alongside a tree nobody was still looking at.
 *
 * Both views stay mounted and toggle with `hidden`, never conditional rendering.
 * That is what preserves the tree's expanded directories and scroll offset:
 * expansion lives in this component's state, scroll offset lives on the DOM node,
 * and unmounting would discard both. `hidden` also removes the tree from the
 * accessibility tree and the tab order, so a keyboard user cannot tab into an
 * invisible tree.
 *
 * Every path here is relative to the session's workdir. The backend resolves it
 * against the journal's record of that directory and refuses anything that
 * escapes, so this component never handles an absolute path except for display.
 */

import dynamic from "next/dynamic";
import { useParams } from "next/navigation";
import { useCallback, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, api, type FileEntry } from "@/lib/api";
import { formatBytes, formatRelative } from "@/lib/format";
import { glyphFor } from "@/lib/file-kind";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { ContextMenu, type MenuItem } from "@/components/context-menu";
import { FileViewer } from "@/components/file-viewer";

/**
 * The full-screen viewer.
 *
 * Split out because it pulls in `Overlay` and is reached only by an explicit
 * click, and kept out of the pane's own chunk for the same reason the pane is
 * kept out of first-load JS.
 */
const FullscreenViewer = dynamic(
  () => import("@/components/file-viewer-overlay").then((m) => m.FileViewerOverlay),
  { ssr: false },
);

export function ArtifactsPane() {
  const params = useParams();
  const sessionId = typeof params?.id === "string" ? params.id : undefined;
  const {
    artifact,
    showArtifact,
    clearArtifact,
    artifactView,
    backToTree,
    artifactFullscreen,
    openArtifactFullscreen,
    closeArtifactFullscreen,
  } = useWorkspaceUI();

  // The selected file lives in shell state, not here: a tool call in the
  // conversation opens a file, and that request has to survive this component's
  // own re-renders and reach it from outside.
  const selected =
    artifact && artifact.sessionId === sessionId ? artifact.path : null;
  const selectedKind =
    artifact && artifact.sessionId === sessionId ? artifact.previewKind : undefined;

  /** Treeitem buttons by path, so Back can restore focus where it left. */
  const rowRefs = useRef(new Map<string, HTMLButtonElement>());

  const registerRow = useCallback((path: string, node: HTMLButtonElement | null) => {
    if (node) rowRefs.current.set(path, node);
    else rowRefs.current.delete(path);
  }, []);

  const handleBack = useCallback(() => {
    backToTree();
    // Focus returns to the row that was activated, not to the top of the tree.
    // Deferred a frame because the tree is `hidden` until this update commits,
    // and a hidden element cannot take focus.
    const path = selected;
    requestAnimationFrame(() => {
      if (path) rowRefs.current.get(path)?.focus();
    });
  }, [backToTree, selected]);

  /**
   * The pane's expand button, so focus can return to it after fullscreen closes.
   *
   * `Overlay` restores focus to whatever was active when it opened, but this
   * button is unmounted while the dialog is up — the pane shows a placeholder
   * instead of a second copy of the viewer — so its `isConnected` guard correctly
   * declines to focus a detached node. Restoring it here, once React has
   * remounted the button, is what keeps the round trip WCAG-conformant.
   */
  const expandRef = useRef<HTMLButtonElement | null>(null);

  const handleCloseFullscreen = useCallback(() => {
    closeArtifactFullscreen();
    requestAnimationFrame(() => expandRef.current?.focus());
  }, [closeArtifactFullscreen]);

  if (!sessionId) {
    return (
      <div className="p-3 text-xs text-gray-500">
        <p className="font-medium text-gray-700">No session open</p>
        <p className="mt-1">
          Open a session to browse the files in its working directory.
        </p>
      </div>
    );
  }

  const viewing = selected !== null && artifactView === "preview";

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* `hidden` rather than unmounting: see the module comment. */}
      <div className={`${viewing ? "hidden" : "flex"} min-h-0 flex-1 flex-col`}>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <FileTree
            sessionId={sessionId}
            selected={selected}
            onSelect={(path, previewKind) =>
              showArtifact({ sessionId, path, previewKind })
            }
            registerRow={registerRow}
            onExpand={openArtifactFullscreen}
          />
        </div>
      </div>

      <div className={`${viewing ? "flex" : "hidden"} min-h-0 flex-1 flex-col bg-white`}>
        {selected && (
          <>
            {artifactFullscreen ? (
              // Both viewers would otherwise render the same content at once.
              // They share a query key so there is no double fetch, but a
              // 3,000-cell table is worth not building twice.
              <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 p-4 text-center">
                <p className="text-[11px] text-gray-500">
                  This file is open in the expanded view.
                </p>
                <button
                  type="button"
                  onClick={handleCloseFullscreen}
                  className="rounded border border-gray-200 px-2 py-0.5 text-[10px] font-medium text-gray-700 hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                >
                  Show it here instead
                </button>
              </div>
            ) : (
              <FileViewer
                sessionId={sessionId}
                path={selected}
                previewKind={selectedKind}
                onBack={handleBack}
                onClose={clearArtifact}
                onExpand={openArtifactFullscreen}
                registerExpand={(node) => {
                  expandRef.current = node;
                }}
              />
            )}
          </>
        )}
      </div>

      {artifactFullscreen && selected && (
        <FullscreenViewer
          sessionId={sessionId}
          path={selected}
          previewKind={selectedKind}
          onClose={handleCloseFullscreen}
        />
      )}
    </div>
  );
}

function FileTree({
  sessionId,
  selected,
  onSelect,
  registerRow,
  onExpand,
}: {
  sessionId: string;
  selected: string | null;
  /** `previewKind` rides along so the viewer needs no probe request. */
  onSelect: (path: string, previewKind?: FileEntry["previewKind"]) => void;
  registerRow: (path: string, node: HTMLButtonElement | null) => void;
  onExpand: () => void;
}) {
  const queryClient = useQueryClient();
  /** Directories the user opened or closed by hand. */
  const [toggled, setToggled] = useState<Set<string>>(new Set());
  /** Directories explicitly closed, so a manual collapse survives re-selection. */
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  // A selected file's ancestors are expanded, so opening a file from a tool call
  // also shows where it lives. Derived during render rather than pushed into
  // state by an effect: the ancestor set is a function of the selected path, and
  // storing it would mean two sources of truth for the same fact.
  const expanded = useMemo(() => {
    const set = new Set(toggled);
    if (selected) {
      let prefix = "";
      for (const part of selected.split("/").slice(0, -1)) {
        prefix = prefix ? `${prefix}/${part}` : part;
        // An ancestor the user deliberately collapsed stays collapsed: the file
        // is still selected, and re-opening it under them would fight the click.
        if (!collapsed.has(prefix)) set.add(prefix);
      }
    }
    return set;
  }, [toggled, collapsed, selected]);

  const root = useQuery({
    queryKey: ["files", sessionId, ""],
    queryFn: () => api.files.tree(sessionId, ""),
  });

  const toggle = useCallback(
    (path: string) => {
      const isOpen = expanded.has(path);
      setToggled((current) => {
        const next = new Set(current);
        if (isOpen) next.delete(path);
        else next.add(path);
        return next;
      });
      setCollapsed((current) => {
        const next = new Set(current);
        if (isOpen) next.add(path);
        else next.delete(path);
        return next;
      });
    },
    [expanded],
  );

  return (
    <div className="px-1.5 py-2">
      <div className="mb-1 flex items-center justify-between gap-2 px-1.5">
        <h2 className="truncate text-[10px] font-semibold uppercase tracking-wider text-gray-500">
          Files
        </h2>
        <button
          type="button"
          onClick={() => void queryClient.invalidateQueries({ queryKey: ["files", sessionId] })}
          title="Reload the file tree"
          className="rounded px-1 text-[10px] text-gray-500 hover:bg-gray-200/60 hover:text-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          Reload
        </button>
      </div>

      {root.data?.root && (
        <p className="mb-1 truncate px-1.5 font-mono text-[10px] text-gray-500" title={root.data.root}>
          {root.data.root}
        </p>
      )}

      <Directory
        sessionId={sessionId}
        path=""
        depth={0}
        expanded={expanded}
        onToggle={toggle}
        selected={selected}
        onSelect={onSelect}
        registerRow={registerRow}
        onExpand={onExpand}
      />
    </div>
  );
}

/**
 * One directory level. Recursive, and each level owns its own query, so an
 * expanded subtree is cached independently and collapsing does not discard it.
 */
function Directory({
  sessionId,
  path,
  depth,
  expanded,
  onToggle,
  selected,
  onSelect,
  registerRow,
  onExpand,
}: {
  sessionId: string;
  path: string;
  depth: number;
  expanded: Set<string>;
  onToggle: (path: string) => void;
  selected: string | null;
  onSelect: (path: string, previewKind?: FileEntry["previewKind"]) => void;
  registerRow: (path: string, node: HTMLButtonElement | null) => void;
  onExpand: () => void;
}) {
  const { data, isPending, error, refetch } = useQuery({
    queryKey: ["files", sessionId, path],
    queryFn: () => api.files.tree(sessionId, path),
    // Only the root and expanded directories are fetched; a collapsed one costs
    // nothing until it is opened.
    enabled: depth === 0 || expanded.has(path),
    staleTime: 15_000,
  });

  if (isPending && (depth === 0 || expanded.has(path))) {
    return (
      <p className="px-1.5 py-1 text-[11px] text-gray-500" style={{ paddingLeft: depth * 10 + 6 }}>
        Loading...
      </p>
    );
  }

  if (error) {
    return (
      <div
        role="alert"
        className="px-1.5 py-1 text-[11px]"
        style={{ paddingLeft: depth * 10 + 6 }}
      >
        {/* The heading names the directory, and the body carries the backend's
            message verbatim. Both are needed: a bare message in a tree reads as
            a property of the tree, which is how an absent *endpoint* came across
            as an absent *directory*. */}
        <p className="text-red-800">{describeTreeError(error, path)}</p>
        <button
          type="button"
          onClick={() => void refetch()}
          className="mt-0.5 rounded text-[10px] text-red-700 underline hover:text-red-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          Retry
        </button>
      </div>
    );
  }

  if (!data) return null;

  return (
    <ul role={depth === 0 ? "tree" : "group"} aria-label={depth === 0 ? "File tree" : undefined}>
      {data.entries.length === 0 && (
        <li
          className="px-1.5 py-1 text-[11px] text-gray-500"
          style={{ paddingLeft: depth * 10 + 6 }}
        >
          Empty
        </li>
      )}
      {data.entries.map((entry) => (
        <TreeNode
          key={entry.path}
          sessionId={sessionId}
          entry={entry}
          depth={depth}
          expanded={expanded}
          onToggle={onToggle}
          selected={selected}
          onSelect={onSelect}
          registerRow={registerRow}
          onExpand={onExpand}
        />
      ))}
      {data.truncated && (
        <li
          className="px-1.5 py-1 text-[10px] text-amber-700"
          style={{ paddingLeft: depth * 10 + 6 }}
        >
          Listing truncated — this directory holds more entries than are shown.
        </li>
      )}
    </ul>
  );
}

/**
 * Phrase a failed directory listing so the reader looks in the right place.
 *
 * The distinction that matters is between "this directory is gone" and "this
 * backend cannot list directories at all". They arrive as the same HTTP status
 * and differ only by error code, and getting them confused is what makes a
 * stale-process problem look like a filesystem one.
 */
function describeTreeError(error: unknown, path: string): string {
  const where = path === "" ? "the working directory" : path;
  if (error instanceof ApiError) {
    switch (error.code) {
      case "FILE_NOT_FOUND":
        return `${where} no longer exists on disk.`;
      case "SESSION_NOT_FOUND":
        return "This session is no longer in the journal.";
      case "POLICY_VIOLATION":
        return `${where} is outside this session's working directory.`;
      // Router-level and envelopeless failures: the endpoint, not the path.
      // `api.ts` has already written the actionable message for these, so it is
      // passed through rather than restated.
      case "NOT_FOUND":
      case "backend_unreachable":
      case "http_404":
        return error.message;
    }
    return `Could not list ${where}: ${error.message}`;
  }
  return `Could not list ${where}.`;
}

function TreeNode({
  sessionId,
  entry,
  depth,
  expanded,
  onToggle,
  selected,
  onSelect,
  registerRow,
  onExpand,
}: {
  sessionId: string;
  entry: FileEntry;
  depth: number;
  expanded: Set<string>;
  onToggle: (path: string) => void;
  selected: string | null;
  onSelect: (path: string, previewKind?: FileEntry["previewKind"]) => void;
  registerRow: (path: string, node: HTMLButtonElement | null) => void;
  onExpand: () => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const isOpen = expanded.has(entry.path);
  const isSelected = selected === entry.path;

  const items: MenuItem[] = useMemo(
    () => [
      {
        label: entry.isDir ? (isOpen ? "Collapse" : "Expand") : "Open preview",
        onSelect: () =>
          entry.isDir ? onToggle(entry.path) : onSelect(entry.path, entry.previewKind),
      },
      ...(entry.isDir
        ? []
        : [
            {
              label: "Open full screen",
              onSelect: () => {
                onSelect(entry.path, entry.previewKind);
                onExpand();
              },
            },
          ]),
      {
        label: "Copy relative path",
        onSelect: () => void navigator.clipboard.writeText(entry.path).catch(() => {}),
        separated: true,
      },
      {
        label: "Copy name",
        onSelect: () => void navigator.clipboard.writeText(entry.name).catch(() => {}),
      },
      // "Open in Default App" and "Reveal in Finder" are deliberately absent.
      // Both require the backend to spawn a process on the host, and this build
      // exposes no such endpoint: a browser page that can invoke `open` on an
      // arbitrary path is a local code-execution surface, and adding it needs a
      // threat-model decision rather than a menu entry.
    ],
    [entry, isOpen, onToggle, onSelect, onExpand],
  );

  return (
    <li role="treeitem" aria-expanded={entry.isDir ? isOpen : undefined} aria-selected={isSelected}>
      <button
        type="button"
        // Registered so Back can return focus to the row that opened the viewer.
        ref={(node) => registerRow(entry.path, node)}
        onClick={() =>
          entry.isDir ? onToggle(entry.path) : onSelect(entry.path, entry.previewKind)
        }
        onContextMenu={(event) => {
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
        title={entry.path}
        style={{ paddingLeft: depth * 10 + 6 }}
        className={`flex w-full items-center gap-1 rounded py-0.5 pr-1.5 text-left text-[11.5px] focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 ${
          isSelected ? "bg-gray-900 text-white" : "text-gray-700 hover:bg-gray-200/60"
        }`}
      >
        <span aria-hidden="true" className={`w-3 shrink-0 ${isSelected ? "text-gray-300" : "text-gray-500"}`}>
          {entry.isDir ? (isOpen ? "▾" : "▸") : ""}
        </span>
        {/* A per-family glyph, so a spreadsheet is recognizable before opening. */}
        <span aria-hidden="true" className="shrink-0 text-[10px]">
          {glyphFor(entry)}
        </span>
        <span className="min-w-0 flex-1 truncate">{entry.name}</span>
        {entry.isSymlink && (
          <span
            aria-label="symbolic link"
            title="Symbolic link, resolving inside the workspace"
            className={`shrink-0 text-[9px] ${isSelected ? "text-gray-300" : "text-gray-500"}`}
          >
            ↗
          </span>
        )}
        {!entry.isDir && entry.size !== undefined && (
          <span
            className={`shrink-0 text-[9px] ${isSelected ? "text-gray-500" : "text-gray-500"}`}
            title={`Modified ${formatRelative(entry.modifiedAtMs)}`}
          >
            {formatBytes(entry.size)}
          </span>
        )}
      </button>

      {entry.isDir && isOpen && (
        <Directory
          sessionId={sessionId}
          path={entry.path}
          depth={depth + 1}
          expanded={expanded}
          onToggle={onToggle}
          selected={selected}
          onSelect={onSelect}
          registerRow={registerRow}
          onExpand={onExpand}
        />
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={items}
          onClose={() => setMenu(null)}
          label={`${entry.name} actions`}
        />
      )}
    </li>
  );
}
