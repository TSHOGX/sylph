"use client";

/**
 * The application frame.
 *
 * # Two layouts, one component tree
 *
 * On a desktop this is three in-flow panes: Workspaces | Conversation | Artifacts,
 * with fixed sidebar widths so they do not shrink as conversation content grows,
 * and only the middle pane scrolling.
 *
 * On a phone that layout cannot work — a 240px sidebar on a 390px viewport leaves
 * ~150px for the conversation *and* the composer — so the conversation takes the
 * full width and the two sidebars become overlay surfaces: a left drawer for
 * sessions, a bottom sheet for files. Which one is right is not a width question,
 * it is what each pane is *for*: sessions are navigation, and files are inspection
 * of the thing currently on screen, which is why the sheet leaves the conversation
 * visible above it.
 *
 * The split is a component swap, not a stylesheet: an overlay surface has to trap
 * focus, close on Escape, and dismiss on navigation, and none of those three is
 * expressible as a CSS rule. `useIsMobile` shares its breakpoint constant with the
 * `md:` classes here so the two boundaries cannot drift — a focus trap running while
 * CSS still lays the pane out in flow would trap focus inside a visible sidebar.
 *
 * # Height
 *
 * `h-dvh`, never `h-screen`. `100vh` is the *large* viewport height: it measures as
 * though the browser's collapsible toolbars were hidden, so a `h-screen` shell is
 * taller than what is visible and the document scrolls to make up the difference —
 * dragging the whole UI up and exposing blank space below the composer. `100dvh`
 * tracks the toolbars as they collapse. The document itself is pinned in
 * `globals.css`; this shell owns all scrolling.
 */

import dynamic from "next/dynamic";
import { usePathname } from "next/navigation";
import { useCallback, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useQuickCreateSession } from "@/lib/create-session";
import { WorkspaceUIProvider, useWorkspaceUI } from "@/lib/workspace-ui";
import { WorkspaceSidebar } from "@/components/workspace-sidebar";
import { useT } from "@/lib/i18n";
import { useShortcutHint } from "@/lib/shortcuts";
import { IconButton, Kbd } from "@/components/ui/button";
import {
  MenuIcon,
  PanelLeftIcon,
  PanelRightIcon,
  SearchIcon,
  SettingsIcon,
} from "@/components/icons";

/**
 * Surfaces that are not on screen at first paint are code-split.
 *
 * The shell lives in the root layout, so a static import here lands in the first-load
 * bundle of *every* route. Measured, importing the three overlays and the Artifacts pane
 * statically is the difference between being over the first-load budget and under it —
 * the single largest lever available (`docs/frontend-architecture.md` § Code splitting).
 * Each is reached by a keypress or a toggle, by which point the chunk has fetched, which
 * is a fraction of the round trip the action itself takes.
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
const WorkspacePicker = dynamic(
  () => import("@/components/workspace-picker").then((m) => m.WorkspacePicker),
  { ssr: false },
);
const AdoptPicker = dynamic(
  () => import("@/components/adopt-picker").then((m) => m.AdoptPicker),
  { ssr: false },
);

/**
 * The mobile surfaces, split for the same reason as the overlays above — with one
 * extra: they are reachable only *below the `md` breakpoint*, so on a desktop they
 * are not merely unopened, they are unreachable. A static import would put the drag
 * gesture, the scrim, and the focus trap into the first-load bundle of every route for
 * every desktop user.
 */
const Drawer = dynamic(() => import("@/components/ui/sheet").then((m) => m.Drawer), {
  ssr: false,
});
const Sheet = dynamic(() => import("@/components/ui/sheet").then((m) => m.Sheet), {
  ssr: false,
});

/**
 * Routes that own the full width and get no shell.
 *
 * Nothing currently qualifies, but the check is kept because the shell is
 * mounted in the root layout: without a hook here, adding a standalone screen
 * later would mean restructuring the layout instead of adding a path.
 */
const BARE_ROUTES: string[] = [];

export function AppShell({ children }: { children: ReactNode }) {
  const pathname = usePathname();

  // Creating a session is a mutation, not a route: every create path here is
  // dialog-free once a directory is known, and `Mod+N` — the one path that does
  // not know one — asks for the directory alone rather than a form. One instance
  // is shared by every surface, so there is a single pending state and a single
  // error banner rather than one per caller.
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
    <WorkspaceUIProvider onQuickStart={quickStart} onCreateSessionIn={createSession}>
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
  const { leftOpen, rightOpen, overlay, drawer, closeDrawer, isMobile } = useWorkspaceUI();
  const t = useT();

  return (
    <div className="flex h-dvh flex-col overflow-hidden bg-canvas text-ink">
      <TopBar />

      {/* One strip for every create path: Quick Start, the palette, the new-session
          shortcut, and
          the sidebar's "New session here" all run the same mutation, so its
          pending and failed states have one place to appear regardless of which
          triggered it.

          Pending is shown, not just failure. Every create path is dialog-free, so
          between the click and the route change there is otherwise no evidence the
          click registered — and creating a session spawns a runner process, which
          is not always fast enough for that to go unnoticed. `aria-live` on a
          `role="status"` region announces it without stealing focus, which a
          dialog would. */}
      {createPending && (
        <p
          role="status"
          aria-live="polite"
          className="shrink-0 border-b border-line bg-sunken px-4 py-1.5 text-2xs text-ink-muted"
        >
          Starting a session...
        </p>
      )}

      {createError != null && (
        <p
          role="alert"
          className="shrink-0 border-b border-danger-line bg-danger-bg px-4 py-2 text-xs text-danger"
        >
          {createError instanceof Error
            ? createError.message
            : t("shell.createFailed")}
        </p>
      )}

      <div className="flex min-h-0 flex-1">
        {/*
          Desktop only. `hidden` rather than conditional rendering, so the sidebar
          keeps its expanded groups and scroll position when toggled back on.

          `md:` on the visibility means the pane is never in flow on a phone even
          if `showLeftSidebar` is true — which it is by default, and which is what
          crushed the conversation to ~150px.
        */}
        <aside
          aria-label={t("shell.workspaces")}
          className={`${leftOpen ? "md:flex" : "md:hidden"} hidden w-60 shrink-0 flex-col border-r border-line bg-sunken`}
        >
          <WorkspaceSidebar />
        </aside>

        <main id="main" className="flex min-w-0 flex-1 flex-col">
          {children}
        </main>

        <aside
          aria-label={t("shell.artifacts")}
          className={`${rightOpen ? "md:flex" : "md:hidden"} hidden w-80 shrink-0 flex-col border-l border-line bg-sunken xl:w-96`}
        >
          <ArtifactsPane />
        </aside>
      </div>

      {/*
        The mobile surfaces. Mounted only while open *and* only on mobile, unlike
        the panes above: a drawer has no state worth preserving between openings —
        it is dismissed by definition — and keeping a `fixed` overlay mounted but
        hidden would leave its focus trap in the tree.
      */}
      {isMobile && drawer === "sessions" && (
        <Drawer title={t("shell.workspaces")} onClose={closeDrawer}>
          {/*
            Any link tap dismisses the drawer, by delegation.

            The route-tagged derivation in `workspace-ui` closes the drawer when the
            *pathname changes*, which covers most taps and the back button — but not
            tapping the session you are already viewing. That is a same-route
            navigation, so the pathname never changes and the drawer stayed open: the
            user taps a row and nothing happens, which reads as a broken row rather
            than as a no-op. Found by the mobile spec on a clean journal, where the
            first row in the drawer *is* the current session; an accumulated journal
            hid it, because some other session sorted first.

            Delegation rather than a callback threaded into the sidebar: it catches
            every link the drawer will ever contain — session rows, archived rows,
            "Browse all history" — without the sidebar needing to know it is inside a
            drawer. `closest("a[href]")` because the tap usually lands on a span
            inside the anchor.
          */}
          <div
            onClick={(event) => {
              if ((event.target as HTMLElement).closest("a[href]")) closeDrawer();
            }}
            className="flex min-h-0 flex-1 flex-col"
          >
            <WorkspaceSidebar />
          </div>
        </Drawer>
      )}

      {isMobile && drawer === "files" && (
        <Sheet title={t("shell.files")} onClose={closeDrawer}>
          <ArtifactsPane />
        </Sheet>
      )}

      {overlay === "search" && <SearchOverlay />}
      {overlay === "palette" && <CommandPalette />}
      {overlay === "settings" && <SettingsPanel />}
      {overlay === "history" && <HistoryOverlay />}
      {overlay === "newSession" && <NewSessionPicker />}
      {overlay === "adopt" && <AdoptPickerSurface />}
    </div>
  );
}

function TopBar() {
  const { toggleLeft, toggleRight, openOverlay, leftOpen, rightOpen, drawer, isMobile } =
    useWorkspaceUI();
  const hint = useShortcutHint();
  const t = useT();

  return (
    <header className="flex h-12 shrink-0 items-center gap-1 border-b border-line bg-canvas px-2 pt-safe md:h-11">
      {/*
        A hamburger on a phone, a pane toggle on a desktop. Not cosmetic: a pane
        glyph depicts a column beside the content, and on a phone there is no such
        column — the surface it opens covers the content. Using the same icon for
        both would describe a layout that does not exist.
      */}
      <IconButton
        label={
          isMobile
            ? drawer === "sessions"
              ? t("shell.closeWorkspaces")
              : t("shell.openWorkspaces")
            : leftOpen
              ? t("shell.hideWorkspaces")
              : t("shell.showWorkspaces")
        }
        hint={hint("toggleLeftSidebar")}
        onClick={toggleLeft}
        pressed={isMobile ? drawer === "sessions" : leftOpen}
        size={isMobile ? 20 : 16}
      >
        {isMobile ? <MenuIcon size={20} /> : <PanelLeftIcon />}
      </IconButton>

      <span className="ml-0.5 text-sm font-semibold tracking-tight">Sylph</span>

      <div className="mx-1 flex min-w-0 flex-1 justify-end md:justify-start">
        {/*
          A button, not an input: it opens the search overlay rather than accepting
          text inline, so two focusable search surfaces never exist.

          Two shapes for one action. The full-width field is the discoverable form
          and it is what a desktop has room for; on a phone the same field would
          consume the entire bar and leave no room for the title or the two pane
          toggles, so it collapses to its icon. The overlay it opens is identical.
        */}
        <button
          type="button"
          onClick={() => openOverlay("search")}
          aria-label={t("shell.search")}
          className="focus-ring tap-target hidden w-full max-w-md items-center gap-2 rounded-lg border border-line bg-sunken px-2.5 py-1 text-left text-2xs text-ink-muted transition-colors hover:border-line-strong hover:bg-raised md:flex"
        >
          <SearchIcon size={14} />
          <span className="flex-1">{t("shell.search")}</span>
          {hint("search") && <Kbd>{hint("search")}</Kbd>}
        </button>
        <IconButton
          label={t("shell.search")}
          hint={hint("search")}
          onClick={() => openOverlay("search")}
          size={20}
          className="md:hidden"
        >
          <SearchIcon size={20} />
        </IconButton>
      </div>

      <IconButton
        label={t("shortcut.openSettings")}
        hint={hint("openSettings")}
        onClick={() => openOverlay("settings")}
        size={isMobile ? 20 : 16}
      >
        <SettingsIcon size={isMobile ? 20 : 16} />
      </IconButton>

      <IconButton
        label={
          isMobile
            ? drawer === "files"
              ? t("shell.closeFiles")
              : t("shell.openFiles")
            : rightOpen
              ? t("shell.hideArtifacts")
              : t("shell.showArtifacts")
        }
        hint={hint("toggleRightSidebar")}
        onClick={toggleRight}
        pressed={isMobile ? drawer === "files" : rightOpen}
        size={isMobile ? 20 : 16}
      >
        <PanelRightIcon size={isMobile ? 20 : 16} />
      </IconButton>
    </header>
  );
}

/**
 * `Mod+N`: choose a workspace, then create there.
 *
 * This replaced a four-field form, and the fields are worth naming because their
 * absence is the design. `workdir` is the only one a session cannot start
 * without, and it is the one a text input served worst — an absolute path typed
 * from memory, with a mistake surfacing as a backend rejection. `agent` is
 * switchable in the composer until the first turn. `model` and `effort` are
 * composer controls on every turn, so choosing them once at creation was strictly
 * less capable. `title` was optional and is derivable from the opening message,
 * which is what the sidebar already labels a session with.
 *
 * So the form's only irreplaceable field is the directory, and a picker is a
 * better control for it than a text box. Creation errors surface in the shell's
 * shared banner, which is why this renders no error state of its own.
 *
 * # Why this opens on known workspaces rather than on the filesystem
 *
 * It used to open `DirectoryPicker` — a raw browse rooted at `$HOME`, navigated by
 * mouse one level at a time. But a new session almost always belongs in a directory
 * this machine has already worked in, and the backend answers exactly that question
 * ranked by recency. Starting from the filesystem made the common case the slow one
 * and the rare case no faster. `WorkspacePicker` keeps the filesystem one click
 * away for a directory that has genuinely never been used.
 */
/**
 * Adopting a session an agent already has.
 *
 * A one-line wrapper for the same reason `NewSessionPicker` is one: the shell owns
 * dismissal, and the picker owns everything else. It navigates on its own, because where
 * an adopted session lands is a property of the session rather than of the shell.
 */
function AdoptPickerSurface() {
  const { closeOverlay } = useWorkspaceUI();
  return <AdoptPicker onClose={closeOverlay} />;
}

function NewSessionPicker() {
  const { closeOverlay, createSessionIn } = useWorkspaceUI();
  const t = useT();

  return (
    <WorkspacePicker
      title={t("shell.newSession.title")}
      confirmLabel={t("shell.newSession.confirm")}
      onClose={closeOverlay}
      onChoose={(path) => {
        closeOverlay();
        createSessionIn(path);
      }}
    />
  );
}
