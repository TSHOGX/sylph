"use client";

/**
 * The three-pane frame: Workspaces | Conversation | Artifacts.
 *
 * This sits in the root layout rather than inside a page, so the sidebars keep
 * their scroll position and open/closed state across navigation. A shell inside
 * a page would remount on every route change and lose both.
 *
 * Panes are laid out with flexbox and fixed sidebar widths rather than a CSS
 * grid with `fr` units: sidebars must not shrink as conversation content grows,
 * and only the middle pane should scroll.
 */

import dynamic from "next/dynamic";
import { useRouter, usePathname } from "next/navigation";
import { useCallback, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type Session } from "@/lib/api";
import { useSettings } from "@/lib/settings";
import { useQuickCreateSession } from "@/lib/create-session";
import { WorkspaceUIProvider, useWorkspaceUI } from "@/lib/workspace-ui";
import { WorkspaceSidebar } from "@/components/workspace-sidebar";
import { formatCombo, useIsMac } from "@/lib/shortcuts";

/**
 * Surfaces that are not on screen at first paint are code-split.
 *
 * The shell lives in the root layout, so a static import here lands in the
 * first-load bundle of *every* route. Measured on this tree: importing the three
 * overlays and the Artifacts pane statically costs 204.3 KB gzipped of first-load
 * JS, over the 200 KB budget Phase 4 established; splitting them brings it back
 * under. Each is reached by a keypress or a toggle, by which point the chunk has
 * fetched — a fraction of the round trip the action itself takes.
 *
 * `ssr: false` on all four: they render nothing until state opens them, so
 * prerendering them produces markup the client immediately discards.
 */
const ArtifactsPane = dynamic(
  () => import("@/components/artifacts-pane").then((m) => m.ArtifactsPane),
  { ssr: false },
);
const SearchOverlay = dynamic(
  () => import("@/components/search-overlay").then((m) => m.SearchOverlay),
  { ssr: false },
);
const CommandPalette = dynamic(
  () => import("@/components/command-palette").then((m) => m.CommandPalette),
  { ssr: false },
);
const SettingsPanel = dynamic(
  () => import("@/components/settings-panel").then((m) => m.SettingsPanel),
  { ssr: false },
);
const HistoryOverlay = dynamic(
  () => import("@/components/history-overlay").then((m) => m.HistoryOverlay),
  { ssr: false },
);

/**
 * Routes that own the full width and get no shell.
 *
 * Nothing currently qualifies, but the check is kept because the shell is
 * mounted in the root layout: without a hook here, adding a standalone screen
 * later would mean restructuring the layout instead of adding a path.
 */
const BARE_ROUTES: string[] = [];

export function AppShell({ children }: { children: ReactNode }) {
  const router = useRouter();
  const pathname = usePathname();

  const goToNewSession = useCallback(() => router.push("/sessions/new"), [router]);

  // Creating a session is a mutation, not a route: the point of both Quick Start
  // and the sidebar's "New session here" is that neither shows a dialog. One
  // instance is shared by every surface, so there is a single pending state and a
  // single error banner rather than one per caller.
  const creator = useQuickCreateSession();

  // The Quick Start directory is resolved server-side; `~` cannot be expanded in
  // a browser, and the backend runs as the user whose home directory is correct.
  const workspaces = useQuery({
    queryKey: ["workspaces"],
    queryFn: () => api.workspaces.list(),
    staleTime: 30_000,
  });
  const quickStartDir = workspaces.data?.quickStartDir;

  const createSession = creator.create;
  const quickStart = useCallback(
    () => createSession(quickStartDir),
    [createSession, quickStartDir],
  );

  if (BARE_ROUTES.includes(pathname)) {
    return <>{children}</>;
  }

  return (
    <WorkspaceUIProvider
      onNewSession={goToNewSession}
      onQuickStart={quickStart}
      onCreateSessionIn={createSession}
    >
      <ShellFrame createPending={creator.isPending} createError={creator.error}>
        {children}
      </ShellFrame>
    </WorkspaceUIProvider>
  );
}

function ShellFrame({
  children,
  createPending,
  createError,
}: {
  children: ReactNode;
  createPending: boolean;
  createError: unknown;
}) {
  const { leftOpen, rightOpen, overlay } = useWorkspaceUI();

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-white text-gray-900">
      <TopBar />

      {/* One banner for every dialog-free create path: Quick Start, the palette,
          and the sidebar's "New session here" all run the same mutation, so a
          failure has one place to appear regardless of which triggered it. */}
      {createError != null && (
        <p role="alert" className="border-b border-red-200 bg-red-50 px-4 py-2 text-sm text-red-800">
          {createError instanceof Error
            ? createError.message
            : "Could not create the session."}
        </p>
      )}

      <div className="flex min-h-0 flex-1">
        {/* `hidden` rather than conditional rendering: the sidebar keeps its
            expanded groups and scroll position when toggled back on. */}
        <aside
          aria-label="Workspaces"
          className={`${leftOpen ? "flex" : "hidden"} w-60 shrink-0 flex-col border-r border-gray-200 bg-gray-50`}
        >
          <WorkspaceSidebar />
        </aside>

        <main id="main" className="flex min-w-0 flex-1 flex-col">
          {children}
        </main>

        <aside
          aria-label="Artifacts"
          className={`${rightOpen ? "flex" : "hidden"} w-80 shrink-0 flex-col border-l border-gray-200 bg-gray-50 xl:w-96`}
        >
          <ArtifactsPane />
        </aside>
      </div>

      {overlay === "search" && <SearchOverlay />}
      {overlay === "palette" && <CommandPalette />}
      {overlay === "settings" && <SettingsPanel />}
      {overlay === "history" && <HistoryOverlay />}
    </div>
  );
}

function TopBar() {
  const { toggleLeft, toggleRight, openOverlay, leftOpen, rightOpen } = useWorkspaceUI();
  const { settings } = useSettings();
  const isMac = useIsMac();

  const hint = useCallback(
    (action: string) => {
      const combo = Object.entries(settings.shortcuts).find(([, value]) => value === action)?.[0];
      return combo ? formatCombo(combo, isMac) : "";
    },
    [settings.shortcuts, isMac],
  );

  return (
    <header className="flex h-11 shrink-0 items-center gap-2 border-b border-gray-200 px-2">
      <IconButton
        label={leftOpen ? "Hide workspaces sidebar" : "Show workspaces sidebar"}
        hint={hint("toggleLeftSidebar")}
        onClick={toggleLeft}
        pressed={leftOpen}
      >
        <SidebarGlyph side="left" />
      </IconButton>

      <span className="ml-1 text-sm font-semibold tracking-tight">Sylph</span>

      <div className="mx-2 flex-1">
        {/* A button, not an input: it opens the search overlay rather than
            accepting text inline, so two focusable search surfaces never exist. */}
        <button
          type="button"
          onClick={() => openOverlay("search")}
          className="flex w-full max-w-md items-center gap-2 rounded-md border border-gray-200 bg-gray-50 px-2.5 py-1 text-left text-xs text-gray-500 hover:border-gray-300 hover:bg-white focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          <span aria-hidden="true">⌕</span>
          <span className="flex-1">Search sessions and archive</span>
          {hint("search") && <Kbd>{hint("search")}</Kbd>}
        </button>
      </div>

      <IconButton
        label="Open settings"
        hint={hint("openSettings")}
        onClick={() => openOverlay("settings")}
      >
        <span aria-hidden="true" className="text-sm">
          ⚙
        </span>
      </IconButton>

      <IconButton
        label={rightOpen ? "Hide artifacts sidebar" : "Show artifacts sidebar"}
        hint={hint("toggleRightSidebar")}
        onClick={toggleRight}
        pressed={rightOpen}
      >
        <SidebarGlyph side="right" />
      </IconButton>
    </header>
  );
}

function IconButton({
  label,
  hint,
  onClick,
  pressed,
  children,
}: {
  label: string;
  hint?: string;
  onClick: () => void;
  /** Present only for toggles, so screen readers announce the current state. */
  pressed?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={pressed}
      title={hint ? `${label} (${hint})` : label}
      className="rounded p-1.5 text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
    >
      {children}
    </button>
  );
}

/** Pane toggle glyph. Inline SVG rather than an icon dependency: two shapes do
 *  not justify a package, and this keeps the first-load budget intact. */
function SidebarGlyph({ side }: { side: "left" | "right" }) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <rect x="1" y="2.5" width="14" height="11" rx="1.5" stroke="currentColor" strokeWidth="1.2" />
      <line
        x1={side === "left" ? 5.5 : 10.5}
        y1="2.5"
        x2={side === "left" ? 5.5 : 10.5}
        y2="13.5"
        stroke="currentColor"
        strokeWidth="1.2"
      />
    </svg>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border border-gray-300 bg-white px-1 py-px font-sans text-[10px] text-gray-500">
      {children}
    </kbd>
  );
}

/** Session label helper shared by the sidebar and the overlays. */
export function sessionSubtitle(session: Session): string {
  return session.agent;
}
