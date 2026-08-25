"use client";

/**
 * Left pane: sessions grouped by the directory they run in.
 *
 * The grouping key is `workdir`, because that is what a session actually is —
 * an agent bound to a directory. Sessions in the Quick Start directory are split
 * into a separate "Quick Sessions" group: they exist so a user can ask something
 * without picking a project, and mixing them into the project list would bury
 * the projects under scratch work.
 *
 * Each group shows live sessions *and* recent archived ones. The live journal
 * only holds sessions this app created, so a directory with years of
 * coding-agent history rendered as "No sessions yet" — the sidebar claimed a
 * workspace was empty while TraceDB held hundreds of sessions for that exact
 * path. Archived rows are read-only and marked `⌗`; `Browse all` opens the
 * paged history browser, because a 240px pane is not where you page a thousand
 * rows.
 */

import Link from "next/link";
import { useParams } from "next/navigation";
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type ArchiveSession, type Session } from "@/lib/api";
import {
  formatRelative,
  originalSessionId,
  sessionLabel,
  shortenHome,
} from "@/lib/format";
import { liveSessionsQuery } from "@/lib/sessions-query";
import { useT } from "@/lib/i18n";
import { useSettings } from "@/lib/settings";
import dynamic from "next/dynamic";

// Code-split: the picker pulls in its own queries and markup, and the sidebar
// renders on first paint. Loading it eagerly would put a modal nobody has opened
// into the first-load bundle, which is within 4 KB of its budget.
const DirectoryPicker = dynamic(
  () => import("@/components/directory-picker").then((m) => m.DirectoryPicker),
  { ssr: false },
);
import { ContextMenu, MenuButton, type MenuItem } from "@/components/ui/menu";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Button, IconButton } from "@/components/ui/button";
import {
  AlertTriangleIcon,
  ArchiveIcon,
  BranchIcon,
  ChevronRightIcon,
  FolderIcon,
  FolderOpenIcon,
  MoreIcon,
  OriginNativeIcon,
  OriginSylphIcon,
  PinIcon,
  PlusIcon,
  RunningIcon,
} from "@/components/icons";

/**
 * Archived sessions fetched per expanded group.
 *
 * Small on purpose. This is a recency teaser that proves the directory has
 * history and gets the user one click from it; browsing is `Browse all`'s job.
 * Each row costs the backend a label lookup, so a large number here would make
 * expanding a group noticeably slower for rows the user has to scroll to see.
 */
const ARCHIVE_PREVIEW = 6;

interface Group {
  path: string;
  name: string;
  sessions: Session[];
  isQuickStart: boolean;
  lastUsedMs: number;
}

export function WorkspaceSidebar() {
  const { settings, update } = useSettings();
  const t = useT();
  const params = useParams();
  const activeId = typeof params?.id === "string" ? params.id : undefined;
  const [picking, setPicking] = useState(false);

  const bindWorkspace = (path: string) => {
    update({
      pinnedWorkspaces: [...(settings.pinnedWorkspaces ?? []), path],
    });
    setPicking(false);
  };

  // Shared with the start screen, which lists the same rows to resume rather than
  // to navigate. One declaration, so the two mount together on `/` as one request.
  const sessionsQuery = useQuery(liveSessionsQuery());

  const workspacesQuery = useQuery({
    queryKey: ["workspaces"],
    queryFn: () => api.workspaces.list(),
    staleTime: 30_000,
  });

  const quickStartDir = workspacesQuery.data?.quickStartDir;
  const home = workspacesQuery.data?.home;

  // Destructured before the memo so its dependencies are the two fields actually
  // read. Referencing `settings.pinnedWorkspaces` inside the callback makes the
  // React Compiler infer a dependency on the whole `settings` object, which would
  // recompute the grouping on every unrelated preference change.
  const { pinnedWorkspaces, workspaceNames } = settings;

  const { quick, projects } = useMemo(() => {
    const sessions = sessionsQuery.data?.sessions ?? [];
    const byPath = new Map<string, Group>();

    for (const session of sessions) {
      const path = session.workdir.replace(/\/+$/, "") || "/";
      let group = byPath.get(path);
      if (!group) {
        group = {
          path,
          name: workspaceNames[path] ?? leafName(path),
          sessions: [],
          isQuickStart: path === quickStartDir,
          lastUsedMs: 0,
        };
        byPath.set(path, group);
      }
      group.sessions.push(session);
      group.lastUsedMs = Math.max(group.lastUsedMs, session.updatedAtMs);
    }

    // Pinned workspaces appear even with no sessions, which is the point of
    // pinning: a project you are about to start work in should be reachable.
    for (const path of pinnedWorkspaces) {
      if (!byPath.has(path)) {
        byPath.set(path, {
          path,
          name: workspaceNames[path] ?? leafName(path),
          sessions: [],
          isQuickStart: path === quickStartDir,
          lastUsedMs: 0,
        });
      }
    }

    const all = [...byPath.values()];
    const quickGroup = all.find((group) => group.isQuickStart);
    const rest = all
      .filter((group) => !group.isQuickStart)
      .sort((a, b) => {
        // Pinned first, then most recently used: pinning is an explicit
        // statement that recency should not bury a workspace.
        const aPinned = pinnedWorkspaces.includes(a.path);
        const bPinned = pinnedWorkspaces.includes(b.path);
        if (aPinned !== bPinned) return aPinned ? -1 : 1;
        return b.lastUsedMs - a.lastUsedMs;
      });

    return { quick: quickGroup, projects: rest };
  }, [sessionsQuery.data, pinnedWorkspaces, workspaceNames, quickStartDir]);

  const [paneMenu, setPaneMenu] = useState<{ x: number; y: number } | null>(null);

  return (
    <>
      {/* On the `<nav>`, and gated on `defaultPrevented` rather than on the event
          target.

          Two earlier versions of this did not work. Putting the handler on the
          scroll container reaches only the area *inside* it, so a right-click on
          the footer or the pane's padding — which is most of the chrome once a few
          sessions are listed — hit nothing. Testing `event.target ===
          event.currentTarget` was worse: the container's child fills it, so the
          target is a descendant on every click and the condition was never true.

          Group headers and session rows call `preventDefault` in their own
          handlers, and the event still bubbles here, so `defaultPrevented` is
          exactly the question "did something more specific already claim this
          click". A row's own menu therefore wins, and everything else in the pane
          falls through to the pane menu. */}
      <nav
        aria-label={t("sidebar.title")}
        className="flex min-h-0 flex-1 flex-col"
        onContextMenu={(event) => {
          if (event.defaultPrevented) return;
          event.preventDefault();
          setPaneMenu({ x: event.clientX, y: event.clientY });
        }}
      >
        <div className="min-h-0 flex-1 pane px-1.5 py-2">
        {sessionsQuery.isPending ? (
          <output aria-live="polite" className="block px-1.5 py-3 text-xs text-ink-muted">
            Loading sessions...
          </output>
        ) : sessionsQuery.error ? (
          <div role="alert" className="px-1.5 py-3 text-xs">
            <p className="font-medium text-danger">Could not load sessions</p>
            <button
              type="button"
              onClick={() => void sessionsQuery.refetch()}
              className="mt-1.5 rounded border border-danger-line bg-raised px-2 py-1 font-medium text-danger hover:bg-danger-bg focus-ring"
            >
              {t("app.retry")}
            </button>
          </div>
        ) : (
          <>
            <GroupSection
              title={t("sidebar.quickSessions")}
              group={
                quick ?? {
                  path: quickStartDir ?? "",
                  // Derived from the resolved path rather than written out: the
                  // directory is configurable, so a literal name here would be
                  // wrong for anyone who set `paths.quick_start_dir`. This is only
                  // the pre-load fallback — the real row carries the server's own
                  // leaf name.
                  name: leafName(quickStartDir ?? ""),
                  sessions: [],
                  isQuickStart: true,
                  lastUsedMs: 0,
                }
              }
              activeId={activeId}
              home={home}
              emptyHint={t("sidebar.quickSessionsHint")}
            />

            <h2 className="mt-3 px-1.5 pb-1 text-3xs font-semibold uppercase tracking-wider text-ink-muted">
              Workspaces
            </h2>
            {projects.length === 0 ? (
              <p className="px-1.5 py-2 text-xs text-ink-muted">
                No project sessions yet. Create one to see its workspace here.
              </p>
            ) : (
              projects.map((group) => (
                <GroupSection
                  key={group.path}
                  group={group}
                  activeId={activeId}
                  home={home}
                  pinned={settings.pinnedWorkspaces.includes(group.path)}
                  onTogglePin={() =>
                    update({
                      pinnedWorkspaces: settings.pinnedWorkspaces.includes(group.path)
                        ? settings.pinnedWorkspaces.filter((p) => p !== group.path)
                        : [...settings.pinnedWorkspaces, group.path],
                    })
                  }
                  onRename={(name) =>
                    update({
                      workspaceNames: name
                        ? { ...settings.workspaceNames, [group.path]: name }
                        : omit(settings.workspaceNames, group.path),
                    })
                  }
                />
              ))
            )}
          </>
        )}
      </div>

      {/* Also offered as a button, not only on right-click: this is the sole entry
          point for binding a directory that has no sessions yet, and a hidden
          gesture is a poor way to expose the only path to a feature. */}
      <div className="shrink-0 border-t border-line p-1.5 pb-safe md:pb-1.5">
        <Button
          variant="ghost"
          onClick={() => setPicking(true)}
          icon={<PlusIcon size={14} />}
          className="w-full justify-start"
        >
          New Workspace...
        </Button>
      </div>

      {paneMenu && (
        <ContextMenu
          x={paneMenu.x}
          y={paneMenu.y}
          // "list", to distinguish it from a *single* workspace's menu. Both open
          // on right-click in the same pane, so an unqualified "Workspace actions"
          // is ambiguous to a screen reader reading only the menu's name.
          label={t("sidebar.listActions")}
          onClose={() => setPaneMenu(null)}
          items={[
            {
              label: t("sidebar.newWorkspaceEllipsis"),
              onSelect: () => {
                setPaneMenu(null);
                setPicking(true);
              },
            },
          ]}
        />
      )}
    </nav>

    {picking && (
      <DirectoryPicker
        title={t("sidebar.newWorkspace")}
        confirmLabel={t("sidebar.bindWorkspace")}
        onChoose={bindWorkspace}
        onClose={() => setPicking(false)}
      />
    )}
    </>
  );
}

function GroupSection({
  group,
  activeId,
  title,
  home,
  emptyHint,
  pinned,
  onTogglePin,
  onRename,
}: {
  group: Group;
  activeId?: string;
  title?: string;
  home?: string;
  emptyHint?: string;
  pinned?: boolean;
  onTogglePin?: () => void;
  onRename?: (name: string) => void;
}) {
  const queryClient = useQueryClient();
  const { browseHistory, createSessionIn } = useWorkspaceUI();
  const t = useT();
  // Groups start expanded: a collapsed-by-default sidebar hides the thing the
  // user came here to click. Collapse is per-group and not persisted, because it
  // is a transient focusing action rather than a preference.
  const [open, setOpen] = useState(true);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  // Only while expanded, and only for a real directory: a collapsed sidebar
  // must not cost a label lookup per row, and the Quick Sessions placeholder has
  // no path until `GET /workspaces` resolves it.
  const archiveQuery = useQuery({
    queryKey: ["archive", "sessions", group.path, ARCHIVE_PREVIEW],
    queryFn: () => api.archive.list({ workdir: group.path, limit: ARCHIVE_PREVIEW }),
    enabled: open && Boolean(group.path),
    // Archived history is settled: it only changes when TraceDB ingests, which
    // is not driven by anything happening in this tab.
    staleTime: 60_000,
  });

  const archived = useMemo(() => {
    const live = new Set(group.sessions.map((session) => session.traceSessionId).filter(Boolean));
    // No fork de-duplication here: this fetches a single page, and the server
    // already resolves a fork pair within one response. Only a surface that
    // accumulates pages — the history overlay — can see a pair split across two.
    return (archiveQuery.data?.sessions ?? []).filter(
      // An imported session is already a live row above; listing it again offers
      // two routes into one conversation.
      (row) => !row.sessionId && !live.has(row.id),
    );
  }, [archiveQuery.data, group.sessions]);

  const running = group.sessions.filter((s) => s.status === "running").length;
  const label = title ?? group.name;
  const archiveAvailable = archiveQuery.data?.available ?? true;
  const hasMoreArchive = Boolean(archiveQuery.data?.nextCursor) || archived.length > 0;

  const items: MenuItem[] = [
    // Creates immediately with the configured default agent and model. A group
    // *is* a directory, so the create form's only required field is already
    // decided by which group was right-clicked; routing there would ask the user
    // to confirm what they just said. "New session here..." below covers the
    // cases where the agent, model, or title needs overriding.
    {
      label: t("sidebar.newSessionHere"),
      // Disabled only for the placeholder Quick Sessions group, whose path is
      // empty until `GET /workspaces` resolves it. Dimmed rather than hidden, so
      // the menu does not change shape between renders.
      disabled: !group.path,
      onSelect: () => createSessionIn(group.path),
    },
    {
      label: t("sidebar.browseHistory"),
      // Separated from the create actions: reading past work and starting new
      // work are different intents, and the archive one is disabled for the
      // placeholder group whose path is not resolved yet.
      separated: true,
      disabled: !group.path,
      onSelect: () => browseHistory(group.path),
    },
    { label: open ? t("action.collapse") : t("action.expand"), separated: true, onSelect: () => setOpen(!open) },
    ...(onTogglePin
      ? [{ label: pinned ? t("sidebar.unpin") : t("sidebar.pin"), onSelect: onTogglePin }]
      : []),
    ...(onRename
      ? [
          {
            label: t("sidebar.renameEllipsis"),
            onSelect: () => {
              const next = window.prompt(t("sidebar.renameLabel"), label);
              // `null` is cancel; an empty string is "clear the override".
              if (next !== null) onRename(next.trim());
            },
          },
        ]
      : []),
    {
      label: t("sidebar.copyPath"),
      onSelect: () => void navigator.clipboard.writeText(group.path).catch(() => {}),
    },
    {
      label: t("action.refresh"),
      onSelect: () => {
        void queryClient.invalidateQueries({ queryKey: ["sessions"] });
        void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      },
    },
  ];

  return (
    <section className="group/group relative">
      <h2 className="sr-only">{label}</h2>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        onContextMenu={(event) => {
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
        aria-expanded={open}
        title={group.path}
        className="tap-row flex w-full items-center gap-1.5 rounded-md py-1.5 pr-8 pl-1.5 text-left text-xs font-medium text-ink-soft hover:bg-hover focus-ring md:py-1"
      >
        <ChevronRightIcon
          size={12}
          className={`shrink-0 text-ink-faint transition-transform ${open ? "rotate-90" : ""}`}
        />
        {/* A workspace *is* a directory, so it gets the same folder icon the file
            tree uses for one — including the open/closed distinction, which here
            doubles as a second cue for the group's own expanded state. */}
        <span className="shrink-0 text-info">
          {open ? <FolderOpenIcon size={15} /> : <FolderIcon size={15} />}
        </span>
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {pinned && (
          <span aria-label="pinned" title={t("sidebar.pinned")} className="shrink-0 text-ink-muted">
            <PinIcon size={11} />
          </span>
        )}
        <span className="flex shrink-0 items-center gap-1 text-3xs text-ink-muted">
          {running > 0 && (
            <span className="flex items-center gap-0.5 text-info" title={`${running} running`}>
              {running}
              <RunningIcon size={9} />
            </span>
          )}
          {/* Live count only. The archive count is unknown without paging the
              whole directory, and a live-plus-preview total would read as a
              complete tally while actually being an arbitrary partial sum. `+`
              says "there is more here" without inventing a number. */}
          <span>
            {group.sessions.length}
            {archived.length > 0 && (
              <span title="plus archived history" className="text-ink-faint">
                +
              </span>
            )}
          </span>
          {!archiveAvailable && (
            <span
              title={t("sidebar.archiveUnavailable")}
              className="text-warning"
            >
              <AlertTriangleIcon size={11} />
            </span>
          )}
        </span>
      </button>

      {/* Pin, rename, browse history, and "new session here" are otherwise
          right-click only, and there is no right-click on a phone. */}
      <span className="absolute top-0 right-0 flex items-center pr-0.5 pt-0.5 opacity-0 group-hover/group:opacity-100 focus-within:opacity-100 pointer-coarse:opacity-100">
        <MenuButton label={`${label} actions`} items={items} align="end">
          {(trigger) => (
            <IconButton label={`${label} actions`} {...trigger}>
              <MoreIcon size={14} />
            </IconButton>
          )}
        </MenuButton>
      </span>

      {open && (
        // Named, because a bare `<ul>` announces only "list" and the sidebar has
        // one per group. The sr-only `<h2>` above titles the section visually for
        // screen readers but does not name this list, so the label is explicit.
        <ul
          aria-label={`Sessions in ${label}`}
          className="mb-1 ml-2 border-l border-line pl-1"
        >
          {group.sessions.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              active={session.id === activeId}
              home={home}
            />
          ))}

          {archived.map((session) => (
            <ArchivedRow key={session.id} session={session} />
          ))}

          {group.sessions.length === 0 && archived.length === 0 && (
            <li className="px-1.5 py-1 text-2xs text-ink-muted">
              {archiveQuery.isFetching ? t("sidebar.loadingHistory") : (emptyHint ?? t("sidebar.noSessions"))}
            </li>
          )}

          {hasMoreArchive && group.path && (
            <li>
              <button
                type="button"
                onClick={() => browseHistory(group.path)}
                className="tap-row flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-left text-2xs text-ink-muted hover:bg-hover hover:text-ink focus-ring"
              >
                <ArchiveIcon size={13} />
                Browse all history
              </button>
            </li>
          )}
        </ul>
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={items}
          onClose={() => setMenu(null)}
          label={`${label} actions`}
        />
      )}
    </section>
  );
}

function SessionRow({
  session,
  active,
  home,
}: {
  session: Session;
  active: boolean;
  home?: string;
}) {
  const queryClient = useQueryClient();
  const t = useT();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  const items: MenuItem[] = [
    {
      label:
        session.traceSessionId || session.nativeSessionId
          ? t("sidebar.copyOriginalId")
          : t("sidebar.copyWebId"),
      onSelect: () =>
        void navigator.clipboard.writeText(originalSessionId(session)).catch(() => {}),
    },
    ...(session.traceSessionId || session.nativeSessionId
      ? [{
          label: t("sidebar.copyWebId"),
          onSelect: () => void navigator.clipboard.writeText(session.id).catch(() => {}),
        }]
      : []),
    {
      label: t("sidebar.copyWorkspacePath"),
      onSelect: () => void navigator.clipboard.writeText(session.workdir).catch(() => {}),
    },
    {
      label: t("sidebar.refreshTranscript"),
      onSelect: () =>
        void queryClient.invalidateQueries({ queryKey: ["session", session.id] }),
    },
  ];

  return (
    <li className="group/row relative">
      <Link
        href={`/sessions/${session.id}`}
        aria-current={active ? "page" : undefined}
        onContextMenu={(event) => {
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
        title={`${sessionLabel(session)} — ${shortenHome(session.workdir, home)}`}
        className={`tap-row flex items-center gap-1.5 rounded-md py-1.5 pr-8 pl-1.5 text-xs focus-ring md:py-1 ${
          active ? "bg-accent text-accent-ink" : "text-ink-soft hover:bg-hover"
        }`}
      >
        {/* Origin is durable policy data, so it stays visible even at this size.
            Two different shapes, filled, not two colours of one dot — the
            distinction has to survive greyscale and colour blindness (WCAG
            1.4.1), because it decides identity and lifecycle policy. */}
        <span
          title={session.origin === "native" ? "native session" : "sylph session"}
          className={`shrink-0 ${
            active
              ? "text-accent-ink-muted"
              : session.origin === "native"
                ? "text-native"
                : "text-success"
          }`}
        >
          {session.origin === "native" ? (
            <OriginNativeIcon size={9} />
          ) : (
            <OriginSylphIcon size={9} />
          )}
        </span>
        <span className="min-w-0 flex-1 truncate">{sessionLabel(session)}</span>
        {session.status === "running" ? (
          <span
            className={`shrink-0 ${active ? "text-accent-ink-muted" : "text-info"}`}
            title={t("sidebar.turnRunning")}
          >
            <RunningIcon size={10} />
          </span>
        ) : (
          // On the dark active row the muted grey inverts: `ink-muted` on the accent
          // surface measures 3.66:1, below the 4.5:1 minimum for this size.
          <span
            className={`shrink-0 text-3xs ${
              active ? "text-accent-ink-muted" : "text-ink-muted"
            }`}
          >
            {formatRelative(session.updatedAtMs).replace(" ago", "")}
          </span>
        )}
      </Link>

      {/* Sibling of the link, not a child: a button inside an anchor is invalid
          HTML, and the nested interactive element makes the row's own activation
          ambiguous. Absolute positioning overlays it without nesting. */}
      <span className="absolute inset-y-0 right-0 flex items-center pr-0.5 opacity-0 group-hover/row:opacity-100 focus-within:opacity-100 pointer-coarse:opacity-100">
        <MenuButton label={`${sessionLabel(session)} actions`} items={items} align="end">
          {(trigger) => (
            <IconButton
              label={`${sessionLabel(session)} actions`}
              className={active ? "text-accent-ink-muted" : ""}
              {...trigger}
            >
              <MoreIcon size={14} />
            </IconButton>
          )}
        </MenuButton>
      </span>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={items}
          onClose={() => setMenu(null)}
          label={`${sessionLabel(session)} actions`}
        />
      )}
    </li>
  );
}

/**
 * One archived session, linking to the read-only view.
 *
 * Visually subordinate to a live row: this is history, and the sidebar's job is
 * still to get the user to work they can continue. `⌗` rather than a colour
 * change carries the distinction, so it survives greyscale and colour-blindness
 * (WCAG 1.4.1).
 */
function ArchivedRow({ session }: { session: ArchiveSession }) {
  const queryClient = useQueryClient();
  const t = useT();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const when = session.endedAtMs ?? session.startedAtMs;
  const label = session.label ?? `Untitled ${session.agent} session`;
  const items: MenuItem[] = [
    {
      label: t("sidebar.copyOriginalId"),
      onSelect: () => void navigator.clipboard.writeText(session.id).catch(() => {}),
    },
    {
      label: t("sidebar.copyWorkspacePath"),
      onSelect: () => void navigator.clipboard.writeText(session.cwd).catch(() => {}),
    },
    {
      label: t("sidebar.refreshTranscript"),
      onSelect: () => {
        void queryClient.invalidateQueries({ queryKey: ["archive", "transcript", session.id] });
        void queryClient.invalidateQueries({ queryKey: ["archive", "sessions"] });
      },
    },
  ];

  return (
    <li className="group/row relative">
      <Link
        href={`/archive/${encodeURIComponent(session.id)}`}
        onContextMenu={(event) => {
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
        title={`${label} — archived ${session.agent} session, read-only`}
        className="tap-row flex items-center gap-1.5 rounded-md py-1.5 pr-8 pl-1.5 text-xs text-ink-muted hover:bg-hover hover:text-ink focus-ring md:py-1"
      >
        {/* An archive box, not a colour change: this is history, and the
            distinction has to survive greyscale (WCAG 1.4.1). */}
        <span title="archived session" className="shrink-0 text-ink-faint">
          <ArchiveIcon size={12} />
        </span>
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {session.subagentCount > 0 && (
          <span
            title={`${session.subagentCount} subagent session${session.subagentCount === 1 ? "" : "s"}`}
            className="flex shrink-0 items-center gap-0.5 text-3xs text-ink-muted"
          >
            <BranchIcon size={10} />
            {session.subagentCount}
          </span>
        )}
        <span className="shrink-0 text-3xs text-ink-muted">
          {formatRelative(when).replace(" ago", "")}
        </span>
      </Link>

      <span className="absolute inset-y-0 right-0 flex items-center pr-0.5 opacity-0 group-hover/row:opacity-100 focus-within:opacity-100 pointer-coarse:opacity-100">
        <MenuButton label={`${label} actions`} items={items} align="end">
          {(trigger) => (
            <IconButton label={`${label} actions`} {...trigger}>
              <MoreIcon size={14} />
            </IconButton>
          )}
        </MenuButton>
      </span>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={items}
          onClose={() => setMenu(null)}
          label={`${label} actions`}
        />
      )}
    </li>
  );
}

function leafName(path: string): string {
  const leaf = path.replace(/\/+$/, "").split("/").filter(Boolean).pop();
  return leaf ?? path;
}

function omit<T>(record: Record<string, T>, key: string): Record<string, T> {
  const next = { ...record };
  delete next[key];
  return next;
}
