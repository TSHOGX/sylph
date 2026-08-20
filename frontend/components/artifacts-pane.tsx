"use client";

/**
 * Right pane: the session's working directory, and a preview of one file.
 *
 * The tree loads one directory per expansion rather than walking the whole tree
 * up front. A repository with 100k files would otherwise spend seconds building
 * a payload whose first screen is all anyone reads, and the pane would be blank
 * until it finished.
 *
 * Every path here is relative to the session's workdir. The backend resolves it
 * against the journal's record of that directory and refuses anything that
 * escapes, so this component never handles an absolute path except for display.
 */

import { useParams } from "next/navigation";
import { useCallback, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, api, type FileEntry } from "@/lib/api";
import { formatBytes, formatRelative } from "@/lib/format";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { ContextMenu, type MenuItem } from "@/components/context-menu";
import { FilePreview } from "@/components/file-preview";

export function ArtifactsPane() {
  const params = useParams();
  const sessionId = typeof params?.id === "string" ? params.id : undefined;
  const { artifact, showArtifact, clearArtifact } = useWorkspaceUI();

  // The selected file lives in shell state, not here: a tool call in the
  // conversation opens a file, and that request has to survive this component's
  // own re-renders and reach it from outside.
  const selected =
    artifact && artifact.sessionId === sessionId ? artifact.path : null;

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

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto">
        <FileTree
          sessionId={sessionId}
          selected={selected}
          onSelect={(path) => showArtifact({ sessionId, path })}
        />
      </div>

      {selected && (
        // Half the pane, so the tree stays navigable while a file is open. A
        // full-height preview would mean losing your place in the tree on every
        // file you look at.
        <div className="flex max-h-[55%] min-h-0 flex-col border-t border-gray-200 bg-white">
          <FilePreview sessionId={sessionId} path={selected} onClose={clearArtifact} />
        </div>
      )}
    </div>
  );
}

function FileTree({
  sessionId,
  selected,
  onSelect,
}: {
  sessionId: string;
  selected: string | null;
  onSelect: (path: string) => void;
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
}: {
  sessionId: string;
  path: string;
  depth: number;
  expanded: Set<string>;
  onToggle: (path: string) => void;
  selected: string | null;
  onSelect: (path: string) => void;
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
}: {
  sessionId: string;
  entry: FileEntry;
  depth: number;
  expanded: Set<string>;
  onToggle: (path: string) => void;
  selected: string | null;
  onSelect: (path: string) => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const isOpen = expanded.has(entry.path);
  const isSelected = selected === entry.path;

  const items: MenuItem[] = useMemo(
    () => [
      {
        label: entry.isDir ? (isOpen ? "Collapse" : "Expand") : "Open preview",
        onSelect: () => (entry.isDir ? onToggle(entry.path) : onSelect(entry.path)),
      },
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
    [entry, isOpen, onToggle, onSelect],
  );

  return (
    <li role="treeitem" aria-expanded={entry.isDir ? isOpen : undefined} aria-selected={isSelected}>
      <button
        type="button"
        onClick={() => (entry.isDir ? onToggle(entry.path) : onSelect(entry.path))}
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
        <span aria-hidden="true" className="shrink-0 text-[10px]">
          {entry.isDir ? "▮" : "▫"}
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
