"use client";

/**
 * Left pane: sessions grouped by the directory they run in.
 *
 * The grouping key is `workdir`, because that is what a session actually is —
 * an agent bound to a directory. Sessions in `~/.agents` are split into a
 * separate "Quick Sessions" group: they exist so a user can ask something
 * without picking a project, and mixing them into the project list would bury
 * the projects under scratch work.
 */

import Link from "next/link";
import { useParams, useRouter } from "next/navigation";
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Session } from "@/lib/api";
import { formatRelative, sessionLabel } from "@/lib/format";
import { useSettings } from "@/lib/settings";
import { ContextMenu, type MenuItem } from "@/components/context-menu";
import { useWorkspaceUI } from "@/lib/workspace-ui";

/** Sessions the sidebar shows. Higher than the list default: the sidebar is the
 *  primary navigation surface, so truncating it hides work rather than paging it. */
const SIDEBAR_LIMIT = 200;

interface Group {
  path: string;
  name: string;
  sessions: Session[];
  isQuickStart: boolean;
  lastUsedMs: number;
}

export function WorkspaceSidebar() {
  const { settings, update } = useSettings();
  const params = useParams();
  const activeId = typeof params?.id === "string" ? params.id : undefined;

  const sessionsQuery = useQuery({
    queryKey: ["sessions", { limit: SIDEBAR_LIMIT }],
    queryFn: () => api.sessions.list({ limit: SIDEBAR_LIMIT }),
    // The sidebar shows running state, which changes without user action here.
    refetchInterval: 10_000,
  });

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

  return (
    <nav aria-label="Sessions by workspace" className="flex min-h-0 flex-1 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-2">
        {sessionsQuery.isPending ? (
          <output aria-live="polite" className="block px-1.5 py-3 text-xs text-gray-500">
            Loading sessions...
          </output>
        ) : sessionsQuery.error ? (
          <div role="alert" className="px-1.5 py-3 text-xs">
            <p className="font-medium text-red-800">Could not load sessions</p>
            <button
              type="button"
              onClick={() => void sessionsQuery.refetch()}
              className="mt-1.5 rounded border border-red-300 bg-white px-2 py-1 font-medium text-red-800 hover:bg-red-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600"
            >
              Retry
            </button>
          </div>
        ) : (
          <>
            <GroupSection
              title="Quick Sessions"
              icon="⚡"
              group={
                quick ?? {
                  path: quickStartDir ?? "",
                  name: ".agents",
                  sessions: [],
                  isQuickStart: true,
                  lastUsedMs: 0,
                }
              }
              activeId={activeId}
              home={home}
              emptyHint="Quick Start puts scratch sessions here."
            />

            <h2 className="mt-3 px-1.5 pb-1 text-[10px] font-semibold uppercase tracking-wider text-gray-500">
              Workspaces
            </h2>
            {projects.length === 0 ? (
              <p className="px-1.5 py-2 text-xs text-gray-500">
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
    </nav>
  );
}

function GroupSection({
  group,
  activeId,
  title,
  icon,
  home,
  emptyHint,
  pinned,
  onTogglePin,
  onRename,
}: {
  group: Group;
  activeId?: string;
  title?: string;
  icon?: string;
  home?: string;
  emptyHint?: string;
  pinned?: boolean;
  onTogglePin?: () => void;
  onRename?: (name: string) => void;
}) {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { createSessionIn } = useWorkspaceUI();
  // Groups start expanded: a collapsed-by-default sidebar hides the thing the
  // user came here to click. Collapse is per-group and not persisted, because it
  // is a transient focusing action rather than a preference.
  const [open, setOpen] = useState(true);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  const running = group.sessions.filter((s) => s.status === "running").length;
  const label = title ?? group.name;

  const items: MenuItem[] = [
    // Creates immediately with the configured default agent and model. A group
    // *is* a directory, so the create form's only required field is already
    // decided by which group was right-clicked; routing there would ask the user
    // to confirm what they just said. "New session here..." below covers the
    // cases where the agent, model, or title needs overriding.
    {
      label: "New session here",
      // Disabled only for the placeholder Quick Sessions group, whose path is
      // empty until `GET /workspaces` resolves it. Dimmed rather than hidden, so
      // the menu does not change shape between renders.
      disabled: !group.path,
      onSelect: () => createSessionIn(group.path),
    },
    {
      label: "New session here...",
      onSelect: () =>
        router.push(`/sessions/new?workdir=${encodeURIComponent(group.path)}`),
    },
    { label: open ? "Collapse" : "Expand", separated: true, onSelect: () => setOpen(!open) },
    ...(onTogglePin
      ? [{ label: pinned ? "Unpin from sidebar" : "Pin to sidebar", onSelect: onTogglePin }]
      : []),
    ...(onRename
      ? [
          {
            label: "Rename workspace...",
            onSelect: () => {
              const next = window.prompt("Workspace display name", label);
              // `null` is cancel; an empty string is "clear the override".
              if (next !== null) onRename(next.trim());
            },
          },
        ]
      : []),
    {
      label: "Copy path",
      onSelect: () => void navigator.clipboard.writeText(group.path).catch(() => {}),
    },
    {
      label: "Refresh",
      onSelect: () => {
        void queryClient.invalidateQueries({ queryKey: ["sessions"] });
        void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      },
    },
  ];

  return (
    <section>
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
        className="flex w-full items-center gap-1 rounded px-1.5 py-1 text-left text-xs font-medium text-gray-700 hover:bg-gray-200/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
      >
        <span aria-hidden="true" className="w-3 shrink-0 text-gray-500">
          {open ? "▾" : "▸"}
        </span>
        {icon && (
          <span aria-hidden="true" className="shrink-0">
            {icon}
          </span>
        )}
        <span className="truncate">{label}</span>
        {pinned && (
          <span aria-label="pinned" title="Pinned" className="shrink-0 text-[10px] text-gray-500">
            ●
          </span>
        )}
        <span className="ml-auto shrink-0 text-[10px] text-gray-500">
          {running > 0 && (
            <span className="mr-1 text-blue-600" title={`${running} running`}>
              {running}▶
            </span>
          )}
          {group.sessions.length}
        </span>
      </button>

      {open && (
        // Named, because a bare `<ul>` announces only "list" and the sidebar has
        // one per group. The sr-only `<h2>` above titles the section visually for
        // screen readers but does not name this list, so the label is explicit.
        <ul
          aria-label={`Sessions in ${label}`}
          className="mb-1 ml-2 border-l border-gray-200 pl-1"
        >
          {group.sessions.length === 0 ? (
            <li className="px-1.5 py-1 text-[11px] text-gray-500">
              {emptyHint ?? "No sessions yet."}
            </li>
          ) : (
            group.sessions.map((session) => (
              <SessionRow
                key={session.id}
                session={session}
                active={session.id === activeId}
                home={home}
              />
            ))
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
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  const items: MenuItem[] = [
    {
      label: "Copy session id",
      onSelect: () => void navigator.clipboard.writeText(session.id).catch(() => {}),
    },
    {
      label: "Copy workspace path",
      onSelect: () => void navigator.clipboard.writeText(session.workdir).catch(() => {}),
    },
    {
      label: "Refresh transcript",
      onSelect: () =>
        void queryClient.invalidateQueries({ queryKey: ["session", session.id] }),
    },
  ];

  return (
    <li>
      <Link
        href={`/sessions/${session.id}`}
        aria-current={active ? "page" : undefined}
        onContextMenu={(event) => {
          event.preventDefault();
          setMenu({ x: event.clientX, y: event.clientY });
        }}
        title={`${sessionLabel(session)} — ${shortenHome(session.workdir, home)}`}
        className={`flex items-center gap-1.5 rounded px-1.5 py-1 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 ${
          active ? "bg-gray-900 text-white" : "text-gray-700 hover:bg-gray-200/60"
        }`}
      >
        {/* Origin is durable policy data, so it stays visible even at this size.
            Shape plus title, not colour alone (WCAG 1.4.1). */}
        <span
          aria-hidden="true"
          title={session.origin === "native" ? "native session" : "sylph session"}
          className={`shrink-0 text-[9px] ${
            active
              ? "text-gray-300"
              : session.origin === "native"
                ? "text-violet-600"
                : "text-emerald-600"
          }`}
        >
          {session.origin === "native" ? "◆" : "●"}
        </span>
        <span className="min-w-0 flex-1 truncate">{sessionLabel(session)}</span>
        {session.status === "running" ? (
          <span
            className={`shrink-0 text-[9px] ${active ? "text-blue-200" : "text-blue-600"}`}
            title="A turn is running"
          >
            ▶
          </span>
        ) : (
          // On the dark active row the muted grey inverts: gray-500 on gray-900
          // measures 3.66:1, below the 4.5:1 minimum for this size.
          <span className={`shrink-0 text-[9px] ${active ? "text-gray-300" : "text-gray-500"}`}>
            {formatRelative(session.updatedAtMs).replace(" ago", "")}
          </span>
        )}
      </Link>

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

/** Abbreviate the home prefix, the way a shell prompt does. */
export function shortenHome(path: string, home?: string): string {
  if (home && path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}

function omit<T>(record: Record<string, T>, key: string): Record<string, T> {
  const next = { ...record };
  delete next[key];
  return next;
}
