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
 * # It manages files, it does not only show them
 *
 * New folder, upload, and delete live in `lib/file-operations.tsx`, mounted once
 * here rather than per row — 2000 rows must not mean 2000 file inputs, and one
 * owner is what makes the confirmation unskippable: a row can only *request* a
 * delete. Its dialogs render outside the `hidden` wrapper below, because a dialog
 * inside a hidden ancestor is invisible *and* unfocusable, so a tool call opening
 * a file in the background would strand it.
 *
 * Expansion state therefore lives here rather than in `FileTree`: a mutation has
 * to be able to reveal its own result, and an upload into a collapsed folder is
 * otherwise indistinguishable from an upload that did nothing.
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
import { useT, type Translate } from "@/lib/i18n";
import { ContextMenu, MenuButton, type MenuItem } from "@/components/ui/menu";
import { FileViewer } from "@/components/file-viewer";
import { IconButton } from "@/components/ui/button";
import { useFileOperations, type FileOperations } from "@/lib/file-operations";
import {
  ChevronRightIcon,
  CopyIcon,
  DownloadIcon,
  ExternalLinkIcon,
  FileCodeIcon,
  FileIcon,
  FileImageIcon,
  FilePdfIcon,
  FileTableIcon,
  FileTextIcon,
  FolderIcon,
  FolderOpenIcon,
  FolderPlusIcon,
  GlobeIcon,
  MaximizeIcon,
  MoreIcon,
  RefreshIcon,
  TrashIcon,
  UploadIcon,
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
  const t = useT();
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

  /**
   * Stop showing a file that no longer exists.
   *
   * A delete leaves the viewer pointed at a path whose next fetch is a 404, which
   * would report "this file no longer exists" as if something had gone wrong —
   * when in fact the user just deleted it deliberately. Clearing the selection
   * returns them to the tree, which is where the result of the delete is visible.
   *
   * Matched by prefix as well as identity: deleting a folder removes everything
   * beneath it, and the open file may be one of those descendants.
   */
  const handleDeleted = useCallback(
    (path: string) => {
      if (selected === path || selected?.startsWith(`${path}/`)) {
        clearArtifact();
        closeArtifactFullscreen();
      }
    },
    [selected, clearArtifact, closeArtifactFullscreen],
  );

  /**
   * Which directories are open.
   *
   * Lifted out of `FileTree` so the operations hook can reach it. A mutation has
   * to be able to *reveal its own result*: uploading into a collapsed folder
   * otherwise looks like nothing happened, because the row it created is inside a
   * subtree that is not rendered. Since `useFileOperations` is mounted here — its
   * dialogs must sit outside the `hidden` wrapper below — the state it needs to
   * touch has to be here too.
   *
   * Two sets rather than one: `toggled` is what the user opened, `collapsed` is
   * what they deliberately closed. The second is what keeps an ancestor of the
   * selected file from re-opening under them; see `expandedFrom`.
   */
  const [toggled, setToggled] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const expanded = useMemo(
    () => expandedFrom(toggled, collapsed, selected),
    [toggled, collapsed, selected],
  );

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

  /**
   * Open a directory and every directory above it.
   *
   * Called after a mutation so its result is on screen. Also clears the path from
   * `collapsed`: the user closed that folder earlier, but they have just put
   * something in it, which supersedes the older intent.
   */
  const revealDirectory = useCallback((path: string) => {
    if (!path) return;
    const ancestors: string[] = [];
    let prefix = "";
    for (const part of path.split("/")) {
      prefix = prefix ? `${prefix}/${part}` : part;
      ancestors.push(prefix);
    }
    setToggled((current) => new Set([...current, ...ancestors]));
    setCollapsed((current) => {
      const next = new Set(current);
      for (const ancestor of ancestors) next.delete(ancestor);
      return next;
    });
  }, []);

  // `?? ""` so the hook order is unconditional. With no session the tree renders
  // nothing, so nothing can reach the operations it exposes.
  const operations = useFileOperations(sessionId ?? "", {
    onDeleted: handleDeleted,
    onWrote: revealDirectory,
  });

  if (!sessionId) {
    return (
      <div className="p-3 text-xs text-ink-muted">
        <p className="font-medium text-ink-soft">{t("files.noSession.title")}</p>
        <p className="mt-1">{t("files.noSession.hint")}</p>
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
            operations={operations}
            expanded={expanded}
            onToggle={toggle}
          />
        </div>
      </div>

      {/* The dialogs and the file input, mounted once for the pane. Outside the
          `hidden` wrapper above: a dialog opened from a row must survive the tree
          being swapped out for the viewer, and a `hidden` ancestor would make it
          both invisible and unfocusable. */}
      {operations.surface}

      <div className={`${viewing ? "flex" : "hidden"} min-h-0 flex-1 flex-col bg-raised`}>
        {selected && (
          <>
            {artifactFullscreen ? (
              // Both viewers would otherwise render the same content at once.
              // They share a query key so there is no double fetch, but a
              // 3,000-cell table is worth not building twice.
              <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 p-4 text-center">
                <p className="text-2xs text-ink-muted">{t("files.openInExpanded")}</p>
                <button
                  type="button"
                  onClick={handleCloseFullscreen}
                  className="rounded border border-line px-2 py-0.5 text-3xs font-medium text-ink-soft hover:bg-sunken focus-ring"
                >
                  {t("files.showHere")}
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
  operations,
  expanded,
  onToggle,
}: {
  sessionId: string;
  selected: string | null;
  /** `previewKind` rides along so the viewer needs no probe request. */
  onSelect: (path: string, previewKind?: FileEntry["previewKind"]) => void;
  registerRow: (path: string, node: HTMLButtonElement | null) => void;
  onExpand: () => void;
  operations: FileOperations;
  /** Expansion is owned by the pane, so a mutation can reveal its own result. */
  expanded: Set<string>;
  onToggle: (path: string) => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();

  const root = useQuery({
    queryKey: ["files", sessionId, ""],
    queryFn: () => api.files.tree(sessionId, ""),
  });

  /** Right-click on empty space: the root's own menu. */
  const [rootMenu, setRootMenu] = useState<{ x: number; y: number } | null>(null);
  const [rootDropping, setRootDropping] = useState(false);

  const rootItems: MenuItem[] = useMemo(
    () => [
      {
        label: t("files.newFolder"),
        icon: <FolderPlusIcon size={13} />,
        onSelect: () => operations.requestMkdir(""),
      },
      {
        label: t("files.uploadHere"),
        icon: <UploadIcon size={13} />,
        onSelect: () => operations.requestUpload(""),
      },
      {
        label: t("sidebar.copyWorkspacePath"),
        icon: <CopyIcon size={13} />,
        separated: true,
        onSelect: () => {
          const absolute = root.data?.root;
          if (absolute) void navigator.clipboard.writeText(absolute).catch(() => {});
        },
        disabled: !root.data?.root,
      },
    ],
    [operations, root.data?.root, t],
  );

  return (
    <div
      // `min-h-full` is what makes the blank area below the last row part of this
      // element. Sized to its content, the container ends at the final row, and
      // the large empty region a user would naturally right-click or drop onto
      // belonged to the scroll wrapper instead — so both gestures did nothing in
      // exactly the space that looks most like "the folder's background".
      className={`min-h-full px-1.5 py-1.5 ${
        rootDropping ? "outline outline-2 -outline-offset-2 outline-accent" : ""
      }`}
      /*
        The empty space below the tree is the workspace root, and treating it that
        way is what makes the pane behave like a file browser rather than a list
        widget: right-click there for the root's actions, drop there to upload into
        it. The header buttons cover the same ground for a keyboard and a phone —
        this is the mouse idiom, added alongside them, not instead.

        A row's own handlers call `stopPropagation`, so a right-click or a drop that
        lands on a row is that row's, never both.
      */
      onContextMenu={(event) => {
        event.preventDefault();
        setRootMenu({ x: event.clientX, y: event.clientY });
      }}
      onDragOver={(event) => {
        if (!event.dataTransfer.types.includes("Files")) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
        setRootDropping(true);
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node)) {
          setRootDropping(false);
        }
      }}
      onDrop={(event) => {
        event.preventDefault();
        setRootDropping(false);
        const files = Array.from(event.dataTransfer.files);
        if (files.length > 0) operations.uploadInto("", files);
      }}
    >
      {rootMenu && (
        <ContextMenu
          x={rootMenu.x}
          y={rootMenu.y}
          items={rootItems}
          onClose={() => setRootMenu(null)}
          label={t("files.rootActions")}
        />
      )}
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
            {t("files.heading")}
          </h2>
          {root.data?.root && (
            <p className="truncate font-mono text-3xs text-ink-muted" title={root.data.root}>
              {root.data.root}
            </p>
          )}
        </div>
        {/*
          Root-level create and upload live in the header, not only in a row menu.
          Adding a file *to the workspace* has no row to right-click: the root is
          the one directory the tree never draws, so without these two buttons the
          only way to reach it would be to pick some child folder and get it wrong.
        */}
        {/*
          Labelled "New folder", not "New folder in the working directory".

          The longer phrasing collided with the directory picker's "Working
          directory" field: `getByLabel` substring-matches, so two buttons in an
          unrelated pane started answering a query meant for one text input, and a
          workspace-shell spec failed. That is the accessible-name collision this
          project has already been bitten by — a screen reader user hears the same
          overlap. The destination needs no spelling out anyway: the root path is
          rendered immediately to the left of these two buttons.
        */}
        <IconButton
          label={t("files.newFolder")}
          onClick={() => operations.requestMkdir("")}
          disabled={operations.busy}
        >
          <FolderPlusIcon size={14} />
        </IconButton>
        <IconButton
          label={t("files.uploadFiles")}
          onClick={() => operations.requestUpload("")}
          disabled={operations.busy}
        >
          <UploadIcon size={14} />
        </IconButton>
        <IconButton
          label={t("files.reloadTree")}
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
        onToggle={onToggle}
        selected={selected}
        onSelect={onSelect}
        registerRow={registerRow}
        onExpand={onExpand}
        operations={operations}
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
  operations,
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
  operations: FileOperations;
}) {
  const t = useT();
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
        {t("action.loading")}
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
        <p className="text-danger">{describeTreeError(error, path, t)}</p>
        <button
          type="button"
          onClick={() => void refetch()}
          className="mt-0.5 rounded text-3xs text-danger underline hover:text-danger focus-ring"
        >
          {t("app.retry")}
        </button>
      </div>
    );
  }

  if (!data) return null;

  return (
    <ul
      role={depth === 0 ? "tree" : "group"}
      aria-label={depth === 0 ? t("files.treeLabel") : undefined}
    >
      {data.entries.length === 0 && (
        <li
          className="px-1.5 py-1 text-2xs text-ink-muted"
          style={{ paddingLeft: depth * 10 + 6 }}
        >
          {/* An empty directory is the one place an upload affordance is *needed*
              rather than convenient: there is no row here to right-click, so
              "Empty" alone would be a dead end. */}
          {t("files.empty")} ·{" "}
          <button
            type="button"
            onClick={() => operations.requestUpload(path)}
            disabled={operations.busy}
            className="rounded text-3xs text-ink-soft underline hover:text-ink focus-ring disabled:opacity-50"
          >
            {t("files.addFiles")}
          </button>
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
          operations={operations}
        />
      ))}
      {data.truncated && (
        <li
          className="px-1.5 py-1 text-3xs text-warning"
          style={{ paddingLeft: depth * 10 + 6 }}
        >
          {t("files.truncated")}
        </li>
      )}
    </ul>
  );
}

/**
 * The set of open directories, given what the user toggled and what is selected.
 *
 * A selected file's ancestors are expanded, so opening a file from a tool call
 * also shows where it lives. Derived during render rather than pushed into state
 * by an effect: the ancestor set is a function of the selected path, and storing
 * it would mean two sources of truth for the same fact.
 */
function expandedFrom(
  toggled: Set<string>,
  collapsed: Set<string>,
  selected: string | null,
): Set<string> {
  const set = new Set(toggled);
  if (selected) {
    let prefix = "";
    for (const part of selected.split("/").slice(0, -1)) {
      prefix = prefix ? `${prefix}/${part}` : part;
      // An ancestor the user deliberately collapsed stays collapsed: the file is
      // still selected, and re-opening it under them would fight the click.
      if (!collapsed.has(prefix)) set.add(prefix);
    }
  }
  return set;
}

/**
 * Open a workspace file in a new browser tab.
 *
 * `noopener` is load-bearing rather than habitual. The raw route serves HTML with
 * `Content-Security-Policy: sandbox`, so the document lands in an opaque origin
 * and cannot reach this one — but `window.open` without it still hands the new
 * document a `window.opener` reference, and the pairing of "sandboxed document"
 * with "live handle back to the app" is exactly the shape that turns into a
 * navigation attack. `noreferrer` is included for the same reason it usually is:
 * the URL carries the file's path, and there is no third party that needs it.
 *
 * A plain anchor with `target="_blank"` would be the more idiomatic form and is
 * not available here: these are menu items, and an anchor inside a `menuitem`
 * either breaks the role or nests interactive elements.
 */
function openInNewTab(url: string) {
  window.open(url, "_blank", "noopener,noreferrer");
}

/**
 * Save a workspace file to disk.
 *
 * The `download` parameter on the URL is what does the work, not a `download`
 * attribute on an element: the attribute is same-origin-only and silently ignored
 * otherwise, whereas `Content-Disposition: attachment` from the backend is
 * authoritative and also carries the filename. Assigning `location` rather than
 * opening a tab means no window is created for a response the browser will hand
 * to the download manager instead of rendering.
 */
function downloadFile(url: string) {
  window.location.assign(url);
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

function describeTreeError(error: unknown, path: string, t: Translate): string {
  const where = path === "" ? t("files.error.workingDirectory") : path;
  if (error instanceof ApiError) {
    switch (error.code) {
      case "FILE_NOT_FOUND":
        return t("files.error.gone", { where });
      case "SESSION_NOT_FOUND":
        return t("files.error.sessionGone");
      case "POLICY_VIOLATION":
        return t("files.error.outside", { where });
      // Router-level and envelopeless failures: the endpoint, not the path.
      // `api.ts` has already written the actionable message for these, so it is
      // passed through rather than restated.
      case "NOT_FOUND":
      case "backend_unreachable":
      case "http_404":
        return error.message;
    }
    return t("files.error.list", { where, message: error.message });
  }
  return t("files.error.listPlain", { where });
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
  operations,
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
  operations: FileOperations;
}) {
  const t = useT();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  /** True while a file drag is over this row, for the drop target's outline. */
  const [dropping, setDropping] = useState(false);
  const isOpen = expanded.has(entry.path);
  const isSelected = selected === entry.path;

  const items: MenuItem[] = useMemo(
    () => [
      {
        label: entry.isDir
          ? t(isOpen ? "action.collapse" : "action.expand")
          : t("files.openPreview"),
        icon: entry.isDir ? <ChevronRightIcon size={13} /> : <FileIcon size={13} />,
        onSelect: () =>
          entry.isDir ? onToggle(entry.path) : onSelect(entry.path, entry.previewKind),
      },
      ...(entry.isDir
        ? [
            {
              label: t("files.newFolder"),
              icon: <FolderPlusIcon size={13} />,
              onSelect: () => operations.requestMkdir(entry.path),
              separated: true,
            },
            {
              label: t("files.uploadHere"),
              icon: <UploadIcon size={13} />,
              onSelect: () => operations.requestUpload(entry.path),
            },
          ]
        : [
            {
              label: t("files.openFullScreen"),
              icon: <MaximizeIcon size={13} />,
              onSelect: () => {
                onSelect(entry.path, entry.previewKind);
                onExpand();
              },
            },
            {
              /*
                The reason this exists: the pane's preview is capped at 256 KB of
                text and 1 MB of image, so a large file is legitimately
                unreadable there. A tab has the browser's own viewer — which
                streams, finds, zooms, and prints — and the backend serves the
                file whole on that route.
              */
              label: t("files.openInNewTab"),
              icon: <ExternalLinkIcon size={13} />,
              onSelect: () => openInNewTab(api.files.rawUrl(sessionId, entry.path)),
            },
            {
              // Offered for every file, not only unrenderable ones. Whether the
              // browser *can* display a type is not the same question as whether
              // the user wants a copy of it, and guessing the answer means a PNG
              // opens a tab when Download was pressed.
              label: t("action.download"),
              icon: <DownloadIcon size={13} />,
              onSelect: () =>
                downloadFile(api.files.rawUrl(sessionId, entry.path, { download: true })),
            },
          ]),
      {
        label: t("files.copyRelativePath"),
        icon: <CopyIcon size={13} />,
        onSelect: () => void navigator.clipboard.writeText(entry.path).catch(() => {}),
        separated: true,
      },
      {
        label: t("files.copyName"),
        icon: <CopyIcon size={13} />,
        onSelect: () => void navigator.clipboard.writeText(entry.name).catch(() => {}),
      },
      {
        label: t("action.delete"),
        icon: <TrashIcon size={13} />,
        danger: true,
        separated: true,
        // Requests, never performs: the confirmation lives in the hook, so no
        // caller can reach a delete without passing through it.
        onSelect: () =>
          operations.requestDelete({
            path: entry.path,
            name: entry.name,
            isDir: entry.isDir,
          }),
      },
      // "Open in Default App" and "Reveal in Finder" are deliberately absent.
      // Both require the backend to spawn a process on the host, and this build
      // exposes no such endpoint: a browser page that can invoke `open` on an
      // arbitrary path is a local code-execution surface, and adding it needs a
      // threat-model decision rather than a menu entry.
    ],
    [entry, isOpen, onToggle, onSelect, onExpand, operations, sessionId, t],
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
      <div
        className="relative"
        /*
          Drop onto a folder uploads into it. This is the gesture people already
          have for "put this file there", and the menu route requires knowing the
          menu exists — but it is an *addition* to that route, never the only one,
          because a drag is unavailable to a keyboard and awkward on a phone.

          `onDragOver` must preventDefault or the browser navigates to the dropped
          file instead, replacing the app with a file:// view of it. Only folders
          accept: dropping on a file has no meaning here, and highlighting a
          target that will refuse is worse than not highlighting.
        */
        onDragOver={
          entry.isDir
            ? (event) => {
                if (!event.dataTransfer.types.includes("Files")) return;
                event.preventDefault();
                // Stopped so the root does not also light up as a target while the
                // pointer is over a folder: two highlighted destinations for one
                // drag is worse than none.
                event.stopPropagation();
                event.dataTransfer.dropEffect = "copy";
                setDropping(true);
              }
            : undefined
        }
        // `relatedTarget` check: dragging across a child element fires leave on
        // the parent, which would flicker the outline off mid-drag.
        onDragLeave={
          entry.isDir
            ? (event) => {
                if (!event.currentTarget.contains(event.relatedTarget as Node)) {
                  setDropping(false);
                }
              }
            : undefined
        }
        onDrop={
          entry.isDir
            ? (event) => {
                event.preventDefault();
                // Stopped, or the same files upload twice: once here and once into
                // the root as the event bubbles.
                event.stopPropagation();
                setDropping(false);
                const files = Array.from(event.dataTransfer.files);
                if (files.length > 0) operations.uploadInto(entry.path, files);
              }
            : undefined
        }
      >
        <button
          type="button"
          // Registered so Back can return focus to the row that opened the viewer.
          ref={(node) => registerRow(entry.path, node)}
          onClick={() =>
            entry.isDir ? onToggle(entry.path) : onSelect(entry.path, entry.previewKind)
          }
          onContextMenu={(event) => {
            event.preventDefault();
            // Stopped, or the tree container also opens the root's menu behind
            // this one — two menus for one right-click.
            event.stopPropagation();
            setMenu({ x: event.clientX, y: event.clientY });
          }}
          title={entry.path}
          // Indent is padding, not a nested margin, so a row's hover and selection
          // background still spans the full pane width at every depth.
          style={{ paddingLeft: depth * 12 + 6 }}
          className={`tap-row flex w-full items-center gap-1.5 rounded-md py-1 pr-8 text-left text-xs focus-ring md:text-2xs ${
            isSelected ? "bg-accent text-accent-ink" : "text-ink-soft hover:bg-hover"
          } ${
            // `outline`, not `ring`: a ring is a box-shadow and the pane clips it.
            dropping ? "outline outline-2 -outline-offset-2 outline-accent" : ""
          } ${operations.uploadingInto === entry.path ? "animate-pulse" : ""}`}
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
              aria-label={t("files.symlink")}
              title={t("files.symlinkTitle")}
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
              title={t("files.modified", { when: formatRelative(entry.modifiedAtMs) })}
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
          <MenuButton
            label={t("files.entryActions", { name: entry.name })}
            items={items}
            align="end"
          >
            {(trigger) => (
              <IconButton
                label={t("files.entryActions", { name: entry.name })}
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
          operations={operations}
        />
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={items}
          onClose={() => setMenu(null)}
          label={t("files.entryActions", { name: entry.name })}
        />
      )}
    </li>
  );
}
