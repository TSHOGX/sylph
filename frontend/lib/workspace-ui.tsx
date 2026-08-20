"use client";

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
import type { ShortcutAction } from "@/lib/settings";

/** At most one overlay is up at a time; they are all modal. */
export type Overlay = "none" | "search" | "palette" | "settings";

/** A file the Artifacts pane should show, addressed inside a session. */
export interface ArtifactTarget {
  sessionId: string;
  /** Path relative to the session workdir. */
  path: string;
  /** Line to scroll to, when the request came from a tool call that named one. */
  line?: number;
}

/**
 * Actions the shell handles centrally. Session-scoped actions (`stopTurn`,
 * `expandAll`) cannot be: they need state that only the open session owns, so
 * the session page registers a handler and the shell forwards to it.
 */
export type SessionActionHandler = (action: ShortcutAction) => boolean;

interface WorkspaceUIValue {
  leftOpen: boolean;
  rightOpen: boolean;
  toggleLeft: () => void;
  toggleRight: () => void;
  setRightOpen: (open: boolean) => void;
  overlay: Overlay;
  openOverlay: (overlay: Overlay) => void;
  closeOverlay: () => void;
  artifact: ArtifactTarget | null;
  /** Show a file in the Artifacts pane, opening the pane if it is hidden. */
  showArtifact: (target: ArtifactTarget) => void;
  clearArtifact: () => void;
  /** Register the open session's shortcut handler. Returns an unsubscriber. */
  registerSessionActions: (handler: SessionActionHandler) => () => void;
  /** Run a shortcut action, for the command palette to reuse the same paths. */
  runAction: (action: ShortcutAction) => void;
  /**
   * Create a session in a known directory and open it, with no dialog.
   *
   * Exposed on the shell context rather than called directly by each surface so
   * that one mutation — and therefore one pending state and one error banner —
   * serves the top bar, the palette, and every sidebar group.
   */
  createSessionIn: (workdir: string | undefined) => void;
}

const WorkspaceUIContext = createContext<WorkspaceUIValue | null>(null);

export function WorkspaceUIProvider({
  children,
  onNewSession,
  onQuickStart,
  onCreateSessionIn,
}: {
  children: ReactNode;
  /** Navigate to the create-session route. Supplied by the shell, which has
   *  the router; this provider stays free of routing concerns. */
  onNewSession: () => void;
  /** Create in the Quick Start directory. The shell supplies it because only
   *  the shell has the backend-resolved path. */
  onQuickStart: () => void;
  onCreateSessionIn: (workdir: string | undefined) => void;
}) {
  const { settings, update } = useSettings();

  // Pane visibility is read straight from settings rather than mirrored into
  // local state. The mirror needed an effect to adopt the stored value after
  // hydration, which cascaded a render on every mount and could disagree with
  // storage if a write failed. `useSettings` is backed by an external store, so
  // a toggle already repaints synchronously.
  const leftOpen = settings.showLeftSidebar;
  const rightOpen = settings.showRightSidebar;

  const [overlay, setOverlay] = useState<Overlay>("none");
  const [artifact, setArtifact] = useState<ArtifactTarget | null>(null);
  const sessionHandlerRef = useRef<SessionActionHandler | null>(null);

  const toggleLeft = useCallback(
    () => update({ showLeftSidebar: !leftOpen }),
    [update, leftOpen],
  );

  const toggleRight = useCallback(
    () => update({ showRightSidebar: !rightOpen }),
    [update, rightOpen],
  );

  const setRightOpenPersisted = useCallback(
    (open: boolean) => update({ showRightSidebar: open }),
    [update],
  );

  const showArtifact = useCallback(
    (target: ArtifactTarget) => {
      setArtifact(target);
      // Opening a file from a tool call is meaningless if the pane stays hidden.
      update({ showRightSidebar: true });
    },
    [update],
  );

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
      switch (action) {
        case "search":
          setOverlay("search");
          return;
        case "commandPalette":
          setOverlay("palette");
          return;
        case "openSettings":
          setOverlay("settings");
          return;
        default:
          setOverlay("none");
      }

      switch (action) {
        case "newSession":
          onNewSession();
          return;
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
    [onNewSession, onQuickStart, toggleLeft, toggleRight],
  );

  // One document-level listener for every shortcut. Per-component listeners
  // would fight over the same combos and make precedence depend on mount order.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return;

      // Escape is not user-rebindable: it closes the top overlay, which every
      // dialog convention and WCAG 2.1.2 expects it to do.
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
      overlay,
      openOverlay: setOverlay,
      closeOverlay: () => setOverlay("none"),
      artifact,
      showArtifact,
      clearArtifact: () => setArtifact(null),
      registerSessionActions,
      runAction,
      createSessionIn: onCreateSessionIn,
    }),
    [
      leftOpen,
      rightOpen,
      toggleLeft,
      toggleRight,
      setRightOpenPersisted,
      overlay,
      artifact,
      showArtifact,
      registerSessionActions,
      runAction,
      onCreateSessionIn,
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
