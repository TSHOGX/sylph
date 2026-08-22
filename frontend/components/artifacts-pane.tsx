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
import { iconFor, type FileIconName } from "@/lib/file-kind";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { ContextMenu, MenuButton, type MenuItem } from "@/components/ui/menu";
import { FileViewer } from "@/components/file-viewer";
import { IconButton } from "@/components/ui/button";
import {
  ChevronRightIcon,
  ExternalLinkIcon,
  FileCodeIcon,
  FileIcon,
  FileImageIcon,
  FilePdfIcon,
  FileTableIcon,
  FileTextIcon,
  FolderIcon,
  FolderOpenIcon,
  GlobeIcon,
  MoreIcon,
  RefreshIcon,
} from "@/components/icons";

/**
 * Icon name to component.
 *
 * Module-level, so the components are never created during render — the rule
 * `react-hooks/static-components` enforces, and the reason `iconFor` returns a name
 * rather than a component reference.
 */
const FILE_ICONS: Record<FileIconName, typeof FileIcon> = {
  table: FileTableIcon,
  image: FileImageIcon,
  pdf: FilePdfIcon,
  web: GlobeIcon,
  prose: FileTextIcon,
  code: FileCodeIcon,
  plain: FileIcon,
};

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
      <div className="p-3 text-xs text-ink-muted">
        <p className="font-medium text-ink-soft">No session open</p>
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
        <div className="min-h-0 flex-1 pane">
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

      <div className={`${viewing ? "flex" : "hidden"} min-h-0 flex-1 flex-col bg-raised`}>
        {selected && (
          <>
            {artifactFullscreen ? (
              // Both viewers would otherwise render the same content at once.
              // They share a query key so there is no double fetch, but a
              // 3,000-cell table is worth not building twice.
              <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 p-4 text-center">
                <p className="text-2xs text-ink-muted">
                  This file is open in the expanded view.
                </p>
                <button
                  type="button"
                  onClick={handleCloseFullscreen}
                  className="rounded border border-line px-2 py-0.5 text-3xs font-medium text-ink-soft hover:bg-sunken focus-ring"
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
    <div className="px-1.5 py-1.5">
      {/*
        Sticky, so the workspace root stays identifiable while scrolling a deep tree.
        It is the only thing on screen naming *which* project this is, and in the
        mobile sheet there is no window title to fall back on.

        The "Files" heading is desktop-only: the sheet already carries that title, and
        a second heading directly beneath it is redundant chrome in the scarcest space.
      */}
      <div className="sticky top-0 z-10 -mx-1.5 mb-1 flex items-center gap-1 border-b border-line bg-sunken px-2.5 py-1 md:bg-transparent">
        <div className="min-w-0 flex-1">
          <h2 className="hidden text-3xs font-semibold uppercase tracking-wider text-ink-muted md:block">
            Files
          </h2>
          {root.data?.root && (
            <p className="truncate font-mono text-3xs text-ink-muted" title={root.data.root}>
              {root.data.root}
            </p>
          )}
        </div>
        <IconButton
          label="Reload the file tree"
          onClick={() => void queryClient.invalidateQueries({ queryKey: ["files", sessionId] })}
        >
          <RefreshIcon size={14} />
        </IconButton>
      </div>

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
      <p className="px-1.5 py-1 text-2xs text-ink-muted" style={{ paddingLeft: depth * 10 + 6 }}>
        Loading...
      </p>
    );
  }

  if (error) {
    return (
      <div
        role="alert"
        className="px-1.5 py-1 text-2xs"
        style={{ paddingLeft: depth * 10 + 6 }}
      >
        {/* The heading names the directory, and the body carries the backend's
            message verbatim. Both are needed: a bare message in a tree reads as
            a property of the tree, which is how an absent *endpoint* came across
            as an absent *directory*. */}
        <p className="text-danger">{describeTreeError(error, path)}</p>
        <button
          type="button"
          onClick={() => void refetch()}
          className="mt-0.5 rounded text-3xs text-danger underline hover:text-danger focus-ring"
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
          className="px-1.5 py-1 text-2xs text-ink-muted"
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
          className="px-1.5 py-1 text-3xs text-warning"
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
/**
 * The icon for one tree row.
 *
 * A folder that is expanded looks expanded, and only the tree knows whether it is —
 * which is why `iconFor` covers files only and this component handles the fork.
 */
function TreeGlyph({ entry, isOpen }: { entry: FileEntry; isOpen: boolean }) {
  if (entry.isDir) {
    return isOpen ? <FolderOpenIcon size={15} /> : <FolderIcon size={15} />;
  }
  const Glyph = FILE_ICONS[iconFor(entry)];
  return <Glyph size={15} />;
}

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
    <li
      role="treeitem"
      aria-expanded={entry.isDir ? isOpen : undefined}
      aria-selected={isSelected}
      // Deliberately not `relative`. An expanded directory's `<li>` contains its
      // whole subtree, so an `inset-y-0` overlay positioned against it stretches down
      // that subtree and centres the overflow button beside a *child* row — which is
      // exactly what a screenshot of the real tree showed. The positioning context is
      // the row wrapper below.
      className="group/row"
    >
      <div className="relative">
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
          // Indent is padding, not a nested margin, so a row's hover and selection
          // background still spans the full pane width at every depth.
          style={{ paddingLeft: depth * 12 + 6 }}
          className={`tap-row flex w-full items-center gap-1.5 rounded-md py-1 pr-8 text-left text-xs focus-ring md:text-2xs ${
            isSelected ? "bg-accent text-accent-ink" : "text-ink-soft hover:bg-hover"
          }`}
        >
          {/*
            The twist reserves its width for a file too, so names line up in a column
            rather than shifting left when a sibling has no children. Rotated rather
            than swapped for a second glyph: one element that turns reads as the same
            control changing state.
          */}
          <span
            className={`w-3 shrink-0 ${isSelected ? "text-accent-ink-muted" : "text-ink-faint"}`}
          >
            {entry.isDir && (
              <ChevronRightIcon
                size={12}
                className={`transition-transform ${isOpen ? "rotate-90" : ""}`}
              />
            )}
          </span>
          {/*
            Directories are tinted and files are not. Folder and file are already
            different silhouettes, so the colour reinforces rather than carries the
            distinction — it survives greyscale (WCAG 1.4.1).
          */}
          <span
            className={`shrink-0 ${
              isSelected ? "text-accent-ink" : entry.isDir ? "text-info" : "text-ink-muted"
            }`}
          >
            <TreeGlyph entry={entry} isOpen={isOpen} />
          </span>
          <span className={`min-w-0 flex-1 truncate ${entry.isDir ? "font-medium" : ""}`}>
            {entry.name}
          </span>
          {entry.isSymlink && (
            <span
              aria-label="symbolic link"
              title="Symbolic link, resolving inside the workspace"
              className={`shrink-0 ${isSelected ? "text-accent-ink-muted" : "text-ink-muted"}`}
            >
              <ExternalLinkIcon size={11} />
            </span>
          )}
          {/* Hidden on a touch pointer: the row is already at its 44px minimum, and
              the size competes with the name for the width a phone does not have. */}
          {!entry.isDir && entry.size !== undefined && (
            <span
              className={`shrink-0 text-3xs pointer-coarse:hidden ${
                isSelected ? "text-accent-ink-muted" : "text-ink-muted"
              }`}
              title={`Modified ${formatRelative(entry.modifiedAtMs)}`}
            >
              {formatBytes(entry.size)}
            </span>
          )}
        </button>

        {/*
          The same actions as the right-click menu, on a button.

          Right-click is the only other way to reach them, and there is no right-click
          on a touch screen — so on a phone every one of these was unreachable, not
          merely awkward. Revealed on hover with a fine pointer to keep the tree quiet;
          always present with a coarse one, where there is no hover to reveal it with.
          A sibling of the row rather than a child, because a button nested inside the
          row's own button is invalid HTML.
        */}
        <span className="absolute inset-y-0 right-0 flex items-center pr-0.5 opacity-0 group-hover/row:opacity-100 focus-within:opacity-100 pointer-coarse:opacity-100">
          <MenuButton label={`${entry.name} actions`} items={items} align="end">
            {(trigger) => (
              <IconButton
                label={`${entry.name} actions`}
                className={isSelected ? "text-accent-ink-muted" : ""}
                {...trigger}
              >
                <MoreIcon size={14} />
              </IconButton>
            )}
          </MenuButton>
        </span>
      </div>

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
