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
 * **One source per group, which used to be two.** `GET /sessions` once held only
 * sessions this app created, so a directory with years of coding-agent history
 * rendered as "No sessions yet" while a separate archive held hundreds of sessions for
 * that exact path — so the pane queried both and merged them, which brought a
 * deduplication step, a translation between two id spaces, and a count that could only
 * be reported as `8+`. With one store and one event vocabulary all three go away: a
 * group is one query, rows are rows, and the count is a count.
 *
 * What survives from that design is the part that was about *presentation* rather than
 * about which store answered: a row is marked by shape rather than colour, and
 * `Browse all` opens the paged history browser, because a 240px pane is not where you
 * page a thousand rows.
 */

import Link from "next/link";
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type Session } from "@/lib/api";
import { formatRelative, sessionLabel, shortenHome } from "@/lib/format";
import { liveSessionsQuery } from "@/lib/sessions-query";
import { workspacesQuery } from "@/lib/workspaces-query";
import { relayName, useEnvironments, type Environments } from "@/lib/environments";
import { useSessionIdParam } from "@/lib/use-session-id-param";
import { useT } from "@/lib/i18n";
import { useSettings } from "@/lib/settings";
import dynamic from "next/dynamic";

// Code-split: the picker pulls in its own queries and markup, and the sidebar
// renders on first paint. Loading it eagerly would put a modal nobody has opened
// into the first-load bundle, which is within 4 KB of its budget.
const WorkspacePicker = dynamic(
  () => import("@/components/workspace-picker").then((m) => m.WorkspacePicker),
  { ssr: false },
);
import { ContextMenu, MenuButton, type MenuItem } from "@/components/ui/menu";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Button, IconButton } from "@/components/ui/button";
import { EnvironmentChip, FidelityMarker } from "@/components/badges";
import {
  ArchiveIcon,
  ChevronRightIcon,
  FolderIcon,
  FolderOpenIcon,
  MoreIcon,
  PinIcon,
  PlusIcon,
  RunningIcon,
} from "@/components/icons";

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
  // Decoded, because this is compared against `session.id` — the decoded triple — to
  // decide which row is current. Read raw, that comparison is silently always false and
  // the active row never highlights, with nothing erroring anywhere
  // (`use-session-id-param`).
  const activeId = useSessionIdParam();
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

  const workspacesResult = useQuery(workspacesQuery());

  // Only to decide whether a row names its machine at all, and to resolve a relaying
  // id into a name. One machine means no chips anywhere — see `useEnvironments`.
  const environments = useEnvironments();

  const quickStartDir = workspacesResult.data?.quickStartDir;
  const home = workspacesResult.data?.home;

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
              environments={environments}
              sessionsPending={sessionsQuery.isPending}
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
                  environments={environments}
                  sessionsPending={sessionsQuery.isPending}
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
          {t("sidebar.newWorkspaceEllipsis")}
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

    {/* The workspace list, not a raw browse: binding a directory that already holds
        archived history is the common case — a project worked on before this UI
        existed — and that directory is absent from the sidebar precisely because
        nothing has bound it yet. The filesystem stays one click away for a folder
        with no history at all. */}
    {picking && (
      <WorkspacePicker
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
  environments,
  sessionsPending,
  pinned,
  onTogglePin,
  onRename,
}: {
  group: Group;
  activeId?: string;
  title?: string;
  home?: string;
  emptyHint?: string;
  environments: Environments;
  /** Whether the one sessions listing is still in flight. */
  sessionsPending: boolean;
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

  // No per-group query at all any more. A group used to fetch archived rows on
  // expansion and merge them against a set of already-imported ids; one store deleted
  // both the fetch and the deduplication, so a group is now a pure function of the one
  // sessions listing the pane already has.
  const running = group.sessions.filter((s) => s.status === "running").length;
  const label = title ?? group.name;

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
          <span aria-label={t("sidebar.pinned")} title={t("sidebar.pinned")} className="shrink-0 text-ink-muted">
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
          {/* A count, not `N+`. It used to carry a `+` because the total was a live
              count plus an arbitrary partial page of archived rows, which would have
              read as a complete tally while being a partial sum. One store makes it
              simply the number of sessions here. */}
          <span>{group.sessions.length}</span>
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
              environments={environments}
            />
          ))}

          {group.sessions.length === 0 && (
            <li className="px-1.5 py-1 text-2xs text-ink-muted">
              {sessionsPending ? t("sidebar.loadingHistory") : (emptyHint ?? t("sidebar.noSessions"))}
            </li>
          )}

          {/* Offered whatever this group holds, because the pane shows the most
              recent rows and a directory can hold over a thousand. It used to be
              gated on an archive preview having produced something, which meant a
              directory whose history had not been ingested offered no way to look. */}
          {group.path && (
            <li>
              <button
                type="button"
                onClick={() => browseHistory(group.path)}
                className="tap-row flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-left text-2xs text-ink-muted hover:bg-hover hover:text-ink focus-ring"
              >
                <ArchiveIcon size={13} />
                {t("sidebar.browseAllHistory")}
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
  environments,
}: {
  session: Session;
  active: boolean;
  home?: string;
  environments: Environments;
}) {
  const queryClient = useQueryClient();
  const t = useT();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  const items: MenuItem[] = [
    {
      // **One entry, because there is one id.** This used to be two — "copy original
      // id" and "copy web id" — because a session had a Sylph-minted UUID *and* an
      // agent-native id, and pasting the wrong one into the wrong tool silently found
      // nothing. Identity is now the triple, so the id in the URL and the id that
      // names this conversation to its vendor are parts of one value: copying it is
      // one action, and the string that goes to the clipboard is what a Sylph route
      // takes verbatim.
      //
      // The *native* id is deliberately not offered separately. It is one field of
      // this session object, so a user who needs it can read it, and a second menu
      // entry would recreate exactly the ambiguity about which id to paste that this
      // deletion removes.
      label: t("sidebar.copySessionId"),
      onSelect: () => void navigator.clipboard.writeText(session.id).catch(() => {}),
    },
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

  const relay = relayName(session.environment, environments);

  return (
    <li className="group/row relative">
      <Link
        href={`/sessions/${encodeURIComponent(session.id)}`}
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
        <span className="min-w-0 flex-1 truncate">{sessionLabel(session)}</span>
        {/* Fidelity is what replaces the origin dot here, and it is a different fact:
            not who started the session but how much its record holds, which is what
            decides whether a duration in it can be trusted. Marked only when it is not
            `stream`, so an ordinary row carries no marker — a marker on every row
            carries no information, which is the one thing the deleted dot got right.

            The marker's own tooltip explains what is missing, so at this size the
            shape alone is enough (WCAG 1.4.1: it is a shape, not a hue). */}
        {session.fidelity !== "stream" && (
          <span
            className={`shrink-0 ${active ? "text-accent-ink-muted" : "text-ink-faint"}`}
          >
            <FidelityMarker value={session.fidelity} />
          </span>
        )}
        {environments.showEnvironment && (
          <EnvironmentChip
            name={environments.nameOf(session.environment) ?? session.environment}
            reachableVia={relay}
          />
        )}
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

function leafName(path: string): string {
  const leaf = path.replace(/\/+$/, "").split("/").filter(Boolean).pop();
  return leaf ?? path;
}

function omit<T>(record: Record<string, T>, key: string): Record<string, T> {
  const next = { ...record };
  delete next[key];
  return next;
}
