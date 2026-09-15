"use client";

import { useSessionIdParam } from "./use-session-id-param";
import { activatePanel, updatePanel, usePanelState } from "./work-panel-state";

/**
 * Shell-wide UI state: which panes are open, which overlay is up, and which
 * artifact the right pane is showing.
 *
 * This lives above the page tree because the shell outlives navigation. A
 * conversation event can ask the Artifacts pane to open a file, and the sidebar
 * toggle must survive moving between sessions — neither works if the state sits
 * inside a page component that unmounts on route change.
 *
 * Pane visibility is mirrored into `Settings` so it persists, but is read from
 * here so a toggle repaints immediately rather than after a storage round trip.
 */

import { usePathname } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useSettings } from "@/lib/settings";
import { comboFromEvent, isTypingTarget } from "@/lib/shortcuts";
import { useIsMobile } from "@/lib/viewport";
import type { ShortcutAction } from "@/lib/settings";
import type { PreviewKind } from "@/lib/api-types";
import { api, type RereadReport } from "@/lib/api";

/**
 * At most one overlay is up at a time; they are all modal.
 *
 * `newSession` is a directory picker, not a form. A session needs a directory and
 * nothing else — the agent is switchable in the composer until the first turn, and
 * model and effort are composer controls for every turn — so the only question
 * worth asking at creation time is "where".
 */
export type Overlay =
  | "none"
  | "search"
  | "palette"
  | "settings"
  | "history"
  | "newSession"
  /**
   * Adopt a session an agent already has, picked from what the machine's native stores
   * hold.
   *
   * A member of this union rather than a route, for the same reason `newSession` is: it is
   * one modal question with one answer, and the surface behind it is where the user
   * already was. Distinct from `newSession` because nothing is started — the transcript
   * exists and is being brought in.
   */
  | "adopt";

/** A file the Artifacts pane should show, addressed inside a session. */
export interface ArtifactTarget {
  sessionId: string;
  /** Path relative to the session workdir. */
  path: string;
  /** Line to scroll to, when the request came from a tool call that named one. */
  line?: number;
  /**
   * Viewer family, when the caller already knew it.
   *
   * Comes from the tree listing, which the server classified. Absent when a tool
   * call opened the file, in which case the viewer falls back to the extension.
   */
  previewKind?: PreviewKind;
}

/** Which half of the Artifacts pane is showing. */
export type ArtifactView = "tree" | "preview";

/**
 * Which mobile surface is open, if any.
 *
 * Separate from `leftOpen`/`rightOpen` rather than reusing them, because they mean
 * different things and must not share a value. Those two are *persisted
 * preferences* — "I work with the sidebar hidden" — and they survive restarts. A
 * drawer is transient: an app that reopens on a phone with a drawer already over
 * the conversation, because that is how the tab was closed a week ago, is broken.
 * Held here rather than in the shell so `showArtifact` can open the file sheet
 * from a tool call in the conversation, which is a different component subtree.
 */
export type MobileDrawer = "none" | "sessions" | "files";

/**
 * Actions the shell handles centrally. Session-scoped actions (`stopTurn`,
 * `collapseAll`) cannot be: they need state that only the open session owns, so
 * the session page registers a handler and the shell forwards to it.
 */
export type SessionActionHandler = (action: ShortcutAction) => boolean;

export type RereadNotice =
  | { state: "pending"; sessionId: string; sessionLabel: string }
  | { state: "success"; sessionId: string; sessionLabel: string; report: RereadReport }
  | { state: "error"; sessionId: string; sessionLabel: string; message: string };

interface WorkspaceUIValue {
  leftOpen: boolean;
  rightOpen: boolean;
  /**
   * Toggle the sessions surface: the persisted pane on a desktop, the transient
   * drawer on a phone. One function for both so every caller — the top bar, the
   * palette, `Mod+B` — expresses the intent and not the mechanism.
   */
  toggleLeft: () => void;
  toggleRight: () => void;
  setRightOpen: (open: boolean) => void;
  /** Open mobile surface. Always `"none"` above the mobile breakpoint. */
  drawer: MobileDrawer;
  openDrawer: (drawer: MobileDrawer) => void;
  closeDrawer: () => void;
  /** True on a phone-sized viewport; see `lib/viewport.ts`. */
  isMobile: boolean;
  overlay: Overlay;
  openOverlay: (overlay: Overlay) => void;
  closeOverlay: () => void;
  artifact: ArtifactTarget | null;
  /** Show a file in the Artifacts pane, opening the pane if it is hidden. */
  showArtifact: (target: ArtifactTarget) => void;
  clearArtifact: () => void;
  /**
   * Whether the pane is browsing the tree or viewing a file.
   *
   * Held here rather than derived from `artifact` because Back must return to the
   * tree *without* deselecting: the row stays highlighted and its ancestors stay
   * expanded, which is what makes returning feel like going back rather than
   * starting over.
   */
  artifactView: ArtifactView;
  /** Return to the tree, keeping the current file selected. */
  backToTree: () => void;
  /** Whether the viewer is showing full-screen over the workspace. */
  artifactFullscreen: boolean;
  openArtifactFullscreen: () => void;
  closeArtifactFullscreen: () => void;
  /** Register the open session's shortcut handler. Returns an unsubscriber. */
  registerSessionActions: (handler: SessionActionHandler) => () => void;
  /**
   * Directory the history browser scopes itself to.
   *
   * Set by whatever established the context — the open session's `workdir`, or
   * the sidebar group whose "Browse all" was clicked. `null` means nothing has,
   * and the overlay then asks the user to pick a workspace instead of guessing.
   */
  activeWorkdir: string | null;
  activeEnvironment: string | null;
  setActiveWorkdir: (workdir: string | null) => void;
  setActiveEnvironment: (environment: string | null) => void;
  /** Open the history browser, optionally scoped to a specific workspace. */
  browseHistory: (workdir?: string, environment?: string | null) => void;
  /**
   * Machine the adopt picker should enumerate, or `null` for "let it choose".
   *
   * Held here rather than passed as a prop because two surfaces open that picker and
   * neither renders it: the shell does. Same shape and same reason as `activeWorkdir`
   * above.
   */
  adoptEnvironment: string | null;
  /**
   * Open the adopt picker, optionally scoped to one machine.
   *
   * The argument is an environment **id**, never a name: a name is mutable metadata and
   * two machines may share one (`docs/architecture.md` § Identity).
   */
  adoptFrom: (environmentId?: string) => void;
  /** Run a shortcut action, for the command palette to reuse the same paths. */
  runAction: (action: ShortcutAction) => void;
  /**
   * Create a session in a known directory and open it, with no dialog.
   *
   * Exposed on the shell context rather than called directly by each surface so
   * that one mutation — and therefore one pending state and one error banner —
   * serves the top bar, the palette, and every sidebar group.
   */
  createSessionIn: (workdir: string | undefined, environmentId?: string | null) => void;
  rereadNotice: RereadNotice | null;
  rereadSession: (sessionId: string, sessionLabel: string) => void;
}

const WorkspaceUIContext = createContext<WorkspaceUIValue | null>(null);

export function WorkspaceUIProvider({
  children,
  onQuickStart,
  onCreateSessionIn,
}: {
  children: ReactNode;
  /** Create in the Quick Start directory. The shell supplies it because only
   *  the shell has the backend-resolved path. */
  onQuickStart: () => void;
  onCreateSessionIn: (workdir: string | undefined, environmentId?: string | null) => void;
}) {
  const routeSessionId = useSessionIdParam();
  const panel = usePanelState(routeSessionId ?? "");
  const { settings, update } = useSettings();
  const queryClient = useQueryClient();

  // Pane visibility is read straight from settings rather than mirrored into
  // local state. The mirror needed an effect to adopt the stored value after
  // hydration, which cascaded a render on every mount and could disagree with
  // storage if a write failed. `useSettings` is backed by an external store, so
  // a toggle already repaints synchronously.
  const leftOpen = settings.showLeftSidebar;
  const rightOpen = settings.showRightSidebar;

  const [overlay, setOverlay] = useState<Overlay>("none");
  const artifact = panel.artifact;
  const artifactView = panel.artifactView;
  const [artifactFullscreen, setArtifactFullscreen] = useState(false);
  const [activeWorkdir, setActiveWorkdir] = useState<string | null>(null);
  const [activeEnvironment, setActiveEnvironment] = useState<string | null>(null);
  const [adoptEnvironment, setAdoptEnvironment] = useState<string | null>(null);
  const sessionHandlerRef = useRef<SessionActionHandler | null>(null);
  const [rereadNotice, setRereadNotice] = useState<RereadNotice | null>(null);
  const rereading = useRef(new Set<string>());

  const rereadSession = useCallback((sessionId: string, sessionLabel: string) => {
    if (rereading.current.has(sessionId)) return;
    rereading.current.add(sessionId);
    setRereadNotice({ state: "pending", sessionId, sessionLabel });
    void api.sessions.reread(sessionId).then(
      ({ reread }) => {
        setRereadNotice({ state: "success", sessionId, sessionLabel, report: reread });
        void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
        void queryClient.invalidateQueries({ queryKey: ["files", sessionId] });
        void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      },
      (error: unknown) => setRereadNotice({
        state: "error", sessionId, sessionLabel,
        message: error instanceof Error ? error.message : "Unknown error.",
      }),
    ).finally(() => rereading.current.delete(sessionId));
  }, [queryClient]);

  const isMobile = useIsMobile();
  const pathname = usePathname();

  /**
   * The open drawer, tagged with the route it was opened on.
   *
   * The route is stored *with* the state rather than watched by an effect, because
   * two rules apply and both are derivations rather than synchronisations:
   *
   * - **Navigating dismisses it.** Tapping a session in the drawer is a navigation,
   *   and leaving the drawer over the conversation the user just chose costs a
   *   second tap on every visit.
   * - **Leaving mobile dismisses it.** A rotation or a window resize must not leave
   *   a `fixed` overlay stranded above a three-pane desktop layout.
   *
   * Written as effects, each of these was a `setState` in an effect body — a
   * cascading render, and what `react-hooks/set-state-in-effect` flags. Both are
   * answerable during render from values already in hand: the current pathname and
   * the current breakpoint. So the state is the raw request and `drawer` below is
   * the derived truth, which is also self-correcting — there is no window in which
   * a stale drawer is renderable.
   */
  const [drawerRequest, setDrawerRequest] = useState<{
    drawer: MobileDrawer;
    at: string;
  }>({ drawer: "none", at: pathname });

  const drawer: MobileDrawer =
    isMobile && drawerRequest.at === pathname ? drawerRequest.drawer : "none";

  const openDrawer = useCallback(
    (next: MobileDrawer) => setDrawerRequest({ drawer: next, at: pathname }),
    [pathname],
  );

  const closeDrawer = useCallback(
    () => setDrawerRequest({ drawer: "none", at: pathname }),
    [pathname],
  );

  const browseHistory = useCallback((workdir?: string, environment?: string | null) => {
    if (workdir) setActiveWorkdir(workdir);
    setActiveEnvironment(environment ?? null);
    setOverlay("history");
    // Called directly from the sidebar as well as through `runAction`, so the
    // one-modal-at-a-time invariant has to be enforced here too — otherwise the
    // history overlay opens behind an already-open full-screen viewer.
    setArtifactFullscreen(false);
  }, []);

  /**
   * Open the adopt picker, scoped to one machine or to the picker's own default.
   *
   * **The scope is written on every call, including the unscoped one**, which is the
   * one line here that is not symmetric with `browseHistory` above. That one keeps a
   * previous `activeWorkdir` when called bare, and is right to: the history browser's
   * scope is a place the user is working, and reopening it where they left off is the
   * useful behaviour. An adopt scope is the opposite — it is an argument belonging to
   * the click that supplied it, so `null` has to *clear* rather than mean "keep". Left
   * conditional, opening the picker from Settings for one machine and later reaching it
   * from the palette would silently re-scope the palette to that machine, and nothing
   * on screen would say which machine was being listed.
   */
  const adoptFrom = useCallback((environmentId?: string) => {
    setAdoptEnvironment(environmentId ?? null);
    setOverlay("adopt");
    // Same reason as `browseHistory`: reached directly from Settings as well as through
    // `runAction`, and an overlay opening behind a full-screen viewer is unreachable.
    setArtifactFullscreen(false);
  }, []);

  // Each toggle dispatches on form factor: a phone has no pane to show or hide, so
  // the same intent has to reach the drawer instead. Doing this here rather than at
  // each call site is what keeps `Mod+B`, the top bar, and the palette in agreement
  // — they were all calling `toggleLeft`, and none of them should have to know.
  const toggleLeft = useCallback(() => {
    if (isMobile) {
      openDrawer(drawer === "sessions" ? "none" : "sessions");
      return;
    }
    update({ showLeftSidebar: !leftOpen });
  }, [isMobile, openDrawer, drawer, update, leftOpen]);

  const toggleRight = useCallback(() => {
    if (isMobile) {
      openDrawer(drawer === "files" ? "none" : "files");
      return;
    }
    update({ showRightSidebar: !rightOpen });
  }, [isMobile, openDrawer, drawer, update, rightOpen]);

  const setRightOpenPersisted = useCallback(
    (open: boolean) => update({ showRightSidebar: open }),
    [update],
  );

  const showArtifact = useCallback(
    (target: ArtifactTarget) => {
      activatePanel(routeSessionId ?? target.sessionId, "files");
      updatePanel(routeSessionId ?? "", { artifact: target, artifactView: "preview" });
      // One atomic update rather than an effect keyed on the target: the action
      // already knows a file was chosen, so deriving "show the preview" from a
      // state change would cascade a render to re-establish a fact we have.

      if (isMobile) {
        // The file sheet, not the persisted pane. Writing `showRightSidebar` here
        // would silently change what the user sees when they next open a desktop
        // browser, because a tap on a path in the transcript is not a statement
        // about how they want their panes arranged.
        openDrawer("files");
        return;
      }
      // Opening a file from a tool call is meaningless if the pane stays hidden.
      update({ showRightSidebar: true });
    },
    [update, isMobile, openDrawer, routeSessionId],
  );

  // Deliberately keeps `artifact` set. The tree derives its expanded ancestors
  // from the selection, so clearing it here would collapse the directories the
  // user navigated through on the way to the file.
  const backToTree = useCallback(() => {
    updatePanel(routeSessionId ?? "", { artifactView: "tree" });
    setArtifactFullscreen(false);
  }, [routeSessionId]);

  const clearArtifact = useCallback(() => {
    updatePanel(routeSessionId ?? "", { artifact: null });
    updatePanel(routeSessionId ?? "", { artifactView: "tree" });
    setArtifactFullscreen(false);
  }, [routeSessionId]);

  // At most one modal surface at a time. Without this, Escape and focus
  // restoration have to reason about which of two stacked dialogs owns them —
  // the same rule `runAction` already applies to the overlays.
  const openArtifactFullscreen = useCallback(() => {
    setArtifactFullscreen(true);
    setOverlay("none");
  }, []);

  const closeArtifactFullscreen = useCallback(() => setArtifactFullscreen(false), []);

  const registerSessionActions = useCallback((handler: SessionActionHandler) => {
    sessionHandlerRef.current = handler;
    return () => {
      // Only clear if this handler is still the current one: a fast route change
      // can mount the next session before the previous one cleans up, and an
      // unconditional clear would erase the new registration.
      if (sessionHandlerRef.current === handler) sessionHandlerRef.current = null;
    };
  }, []);

  const runAction = useCallback(
    (action: ShortcutAction) => {
      // Every action except the three that open an overlay dismisses whatever is
      // open first. Running a command from the palette and being left staring at
      // the palette is the wrong outcome — and it hides the effect of the command
      // that just ran. A no-op when nothing is open, so shortcut dispatch is
      // unaffected.
      //
      // The fullscreen viewer is dismissed whichever branch runs, which is why
      // that call sits outside the switch rather than being repeated in each
      // arm: the one-modal-at-a-time invariant has to hold for overlays added
      // later too, and a per-arm call is a rule every new `case` can forget.
      // An overlay opening behind a full-screen viewer would be unreachable.
      setArtifactFullscreen(false);

      switch (action) {
        case "search":
          setOverlay("search");
          return;
        case "commandPalette":
          setOverlay("palette");
          return;
        case "browseWorkspaceHistory":
          setOverlay("history");
          return;
        case "openSettings":
          setOverlay("settings");
          return;
        case "newSession":
          setOverlay("newSession");
          return;
        case "adoptSession":
          // Through `adoptFrom` with no argument rather than `setOverlay` directly, so
          // the keyboard and palette route *clears* any scope a Settings click left
          // behind. Setting the overlay alone would inherit it, and the palette would
          // then list one machine with nothing saying so.
          adoptFrom();
          return;
        default:
          setOverlay("none");
      }

      switch (action) {
        case "quickStart":
          onQuickStart();
          return;
        case "toggleLeftSidebar":
          toggleLeft();
          return;
        case "toggleRightSidebar":
          toggleRight();
          return;
        default:
          // Session-scoped: forwarded to whichever session is open, and simply
          // ignored when none is.
          sessionHandlerRef.current?.(action);
      }
    },
    [onQuickStart, toggleLeft, toggleRight, adoptFrom],
  );

  // One document-level listener for every shortcut. Per-component listeners
  // would fight over the same combos and make precedence depend on mount order.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return;

      // Escape is not user-rebindable: it closes the top overlay, which every
      // dialog convention and WCAG 2.1.2 expects it to do.
      //
      // The drawer and sheet stop Escape themselves (see `useDialog`), so this only
      // ever runs when neither is open — it is the fallback for a key press that
      // reached the document, not a second handler competing with them.
      if (event.key === "Escape") {
        setOverlay((current) => (current === "none" ? current : "none"));
        return;
      }

      const combo = comboFromEvent(event);
      const action = settings.shortcuts[combo];
      if (!action) return;

      // A bare-key binding (Shift+E) must not steal a keystroke from the
      // composer. Mod-prefixed combos are global on purpose.
      if (!combo.startsWith("Mod+") && isTypingTarget(event.target)) return;

      event.preventDefault();
      runAction(action);
    }

    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [settings.shortcuts, runAction]);

  const value = useMemo(
    () => ({
      leftOpen,
      rightOpen,
      toggleLeft,
      toggleRight,
      setRightOpen: setRightOpenPersisted,
      drawer,
      openDrawer,
      closeDrawer,
      isMobile,
      overlay,
      openOverlay: setOverlay,
      closeOverlay: () => setOverlay("none"),
      artifact,
      showArtifact,
      clearArtifact,
      artifactView,
      backToTree,
      artifactFullscreen,
      openArtifactFullscreen,
      closeArtifactFullscreen,
      registerSessionActions,
      activeWorkdir,
      activeEnvironment,
      setActiveWorkdir,
      setActiveEnvironment,
      browseHistory,
      adoptEnvironment,
      adoptFrom,
      runAction,
      createSessionIn: onCreateSessionIn,
      rereadNotice,
      rereadSession,
    }),
    [
      leftOpen,
      rightOpen,
      toggleLeft,
      toggleRight,
      setRightOpenPersisted,
      drawer,
      openDrawer,
      closeDrawer,
      isMobile,
      overlay,
      artifact,
      showArtifact,
      clearArtifact,
      artifactView,
      backToTree,
      artifactFullscreen,
      openArtifactFullscreen,
      closeArtifactFullscreen,
      registerSessionActions,
      activeWorkdir,
      activeEnvironment,
      browseHistory,
      adoptEnvironment,
      adoptFrom,
      runAction,
      onCreateSessionIn,
      rereadNotice,
      rereadSession,
    ],
  );

  return <WorkspaceUIContext.Provider value={value}>{children}</WorkspaceUIContext.Provider>;
}

export function useWorkspaceUI(): WorkspaceUIValue {
  const context = useContext(WorkspaceUIContext);
  if (!context) {
    throw new Error("useWorkspaceUI must be used inside WorkspaceUIProvider");
  }
  return context;
}
