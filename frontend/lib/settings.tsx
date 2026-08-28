"use client";

/**
 * User preferences, persisted in `localStorage`.
 *
 * These are client-side by design. Every value here is a display or input
 * preference — which agent a new session defaults to, how dense the timeline
 * is, what `Cmd+K` is bound to — and none of it affects what the backend does
 * with a session. Storing them server-side would mean a schema migration for
 * every new toggle, and a round trip before the first paint.
 *
 * The trade-off is that preferences do not follow a user to another browser.
 * That is acceptable for a loopback-only, single-user app; it stops being
 * acceptable the moment a non-loopback deployment adds real accounts, which is
 * already gated behind a separate authentication change.
 */

import { useCallback, useSyncExternalStore } from "react";
// Both imports reach for the *pure* half of their module on purpose: `lib/i18n`
// and `lib/shortcuts` each read this store, so importing either here would be a
// cycle. See the notes at the top of `i18n/locale.ts` and `combo.ts`.
import { DEFAULT_LOCALE, isLocale, type Locale } from "@/lib/i18n/locale";
import { pruneReserved } from "@/lib/combo";

export type Density = "compact" | "normal" | "comfortable";

/**
 * Colour theme. `system` follows `prefers-color-scheme` and keeps following it,
 * so a machine on a light-at-day schedule changes with it.
 */
export type Theme = "light" | "dark" | "system";

/** Actions a keyboard shortcut can be bound to. */
export type ShortcutAction =
  | "search"
  | "commandPalette"
  | "browseWorkspaceHistory"
  | "newSession"
  | "quickStart"
  | "toggleLeftSidebar"
  | "toggleRightSidebar"
  | "openSettings"
  | "stopTurn"
  | "expandAll"
  | "collapseAll"
  | "findInSession";

export interface Settings {
  /** Agent preselected in the create dialog and used by Quick Start. */
  defaultAgent: string;
  /** Model flag applied to new sessions, per agent. */
  defaultModels: Record<string, string>;
  density: Density;
  /** Colour theme. Applied to `<html data-theme>`; see `lib/theme.ts`. */
  theme: Theme;
  /** UI language. Applied to `<html lang>`; see `lib/i18n`. */
  locale: Locale;
  /**
   * Whether a session started from this app gets Sylph's identity.
   *
   * This one is unlike its neighbours: every other field here is a display
   * preference the backend never sees, while this is sent as `injectIdentity` on
   * `POST /sessions` and stored on the row. It lives here because it is a
   * *default for new sessions* — the same kind of thing as `defaultAgent`, which
   * is also a preference that becomes durable session data at creation.
   *
   * It cannot be anything but a creation-time default: identity decides what a
   * transcript was produced by, so changing it later would misdescribe history the
   * agent already wrote. `PATCH /sessions` refuses the field for that reason, and an
   * open session keeps whatever it was created with.
   */
  injectSoul: boolean;
  /** Show thinking blocks expanded on arrival. Off by default: reasoning is
   *  debugging detail, not primary content. */
  expandThinking: boolean;
  /** Expand tool output on success. Errors always expand regardless. */
  expandToolOutput: boolean;
  /** Combo string -> action. See `lib/shortcuts.ts` for the combo grammar. */
  shortcuts: Record<string, ShortcutAction>;
  /** Left sidebar visible. Persisted so a hidden sidebar stays hidden. */
  showLeftSidebar: boolean;
  showRightSidebar: boolean;
  /** Workspaces pinned to the sidebar even with no sessions. */
  pinnedWorkspaces: string[];
  /** Display-name overrides, keyed by absolute workspace path. */
  workspaceNames: Record<string, string>;
}

/**
 * Default bindings.
 *
 * `Cmd+K` searches and `Cmd+P` opens the command palette, following VS Code,
 * Linear, and Raycast. The earlier draft bound both to search, which wasted the
 * more discoverable of the two shortcuts on a duplicate.
 *
 * `Cmd+Shift+K` browses one workspace's archived history, deliberately adjacent
 * to `Cmd+K`: both answer "find a session", and the shifted form is the one
 * scoped to where you are working.
 *
 * `Cmd+Shift+O` creates a session and `Cmd+J` quick-starts one. Neither is the
 * obvious choice — `Cmd+N` is — and that is the point: see `RESERVED_COMBOS` in
 * `lib/shortcuts.ts` for why `Cmd+N` cannot work in a browser tab. `Cmd+Shift+O`
 * follows ChatGPT and Claude's own "new chat" binding, so it is what a user of
 * those arrives expecting.
 */
export const DEFAULT_SHORTCUTS: Record<string, ShortcutAction> = {
  "Mod+k": "search",
  "Mod+Shift+k": "browseWorkspaceHistory",
  "Mod+p": "commandPalette",
  "Mod+Shift+o": "newSession",
  "Mod+j": "quickStart",
  "Mod+b": "toggleLeftSidebar",
  "Mod+\\": "toggleRightSidebar",
  "Mod+,": "openSettings",
  "Mod+.": "stopTurn",
  "Mod+f": "findInSession",
  "Shift+e": "expandAll",
  "Shift+c": "collapseAll",
};

export const DEFAULT_SETTINGS: Settings = {
  defaultAgent: "",
  defaultModels: {},
  density: "normal",
  theme: "system",
  locale: DEFAULT_LOCALE,
  // Sylph identity on by default: this is Sylph's own interface, and the
  // backend's own default for a session created without the field.
  injectSoul: true,
  expandThinking: false,
  expandToolOutput: false,
  shortcuts: DEFAULT_SHORTCUTS,
  showLeftSidebar: true,
  showRightSidebar: false,
  pinnedWorkspaces: [],
  workspaceNames: {},
};

/**
 * Exported because the theme has to be applied *before first paint* to avoid a
 * white flash on a dark-mode phone, which means an inline script in the document
 * head reads this key directly — before React, and therefore before this module,
 * exists. Two spellings of the key would make the pre-paint theme and the
 * in-app theme disagree, so there is one constant and `lib/theme.ts` builds the
 * script from it.
 */
export const STORAGE_KEY = "sylph-web.settings.v1";

interface SettingsContextValue {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  reset: () => void;
}

/**
 * Merge stored JSON over the defaults, field by field.
 *
 * A stored blob from an older build is missing keys a newer one needs, and a
 * hand-edited one can hold anything. Spreading the parsed object wholesale would
 * let `density: 42` through and break rendering, so each field is checked
 * against its own shape and a bad value falls back rather than propagating.
 */
function hydrate(raw: string | null): Settings {
  if (!raw) return DEFAULT_SETTINGS;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return DEFAULT_SETTINGS;
  }
  if (typeof parsed !== "object" || parsed === null) return DEFAULT_SETTINGS;
  const stored = parsed as Partial<Settings>;

  const densities: Density[] = ["compact", "normal", "comfortable"];
  const themes: Theme[] = ["light", "dark", "system"];

  return {
    defaultAgent:
      typeof stored.defaultAgent === "string" ? stored.defaultAgent : DEFAULT_SETTINGS.defaultAgent,
    defaultModels: isStringRecord(stored.defaultModels)
      ? stored.defaultModels
      : DEFAULT_SETTINGS.defaultModels,
    density:
      typeof stored.density === "string" && densities.includes(stored.density as Density)
        ? (stored.density as Density)
        : DEFAULT_SETTINGS.density,
    theme:
      typeof stored.theme === "string" && themes.includes(stored.theme as Theme)
        ? (stored.theme as Theme)
        : DEFAULT_SETTINGS.theme,
    // A locale that was removed from the app, or a hand-edited value, falls back
    // to English rather than leaving every string looked up in a dictionary that
    // does not exist.
    locale: isLocale(stored.locale) ? stored.locale : DEFAULT_SETTINGS.locale,
    injectSoul:
      typeof stored.injectSoul === "boolean" ? stored.injectSoul : DEFAULT_SETTINGS.injectSoul,
    expandThinking:
      typeof stored.expandThinking === "boolean"
        ? stored.expandThinking
        : DEFAULT_SETTINGS.expandThinking,
    expandToolOutput:
      typeof stored.expandToolOutput === "boolean"
        ? stored.expandToolOutput
        : DEFAULT_SETTINGS.expandToolOutput,
    // Custom bindings layer over the defaults rather than replacing them, so a
    // shortcut added in a later release still works for an existing user.
    //
    // Then pruned, which is a migration and not merely a filter. A settings write
    // persists the whole merged map, so anyone who had ever opened the shortcuts
    // panel has `Mod+n` stored from when it was a default — and that binding
    // cannot fire, because the browser takes the keystroke first. Left in place it
    // would keep opening a new browser window with the app's dialog behind it,
    // and would shadow the replacement binding in the panel's reverse lookup.
    shortcuts: isStringRecord(stored.shortcuts)
      ? pruneReserved({
          ...DEFAULT_SHORTCUTS,
          ...(stored.shortcuts as Record<string, ShortcutAction>),
        })
      : DEFAULT_SHORTCUTS,
    showLeftSidebar:
      typeof stored.showLeftSidebar === "boolean"
        ? stored.showLeftSidebar
        : DEFAULT_SETTINGS.showLeftSidebar,
    showRightSidebar:
      typeof stored.showRightSidebar === "boolean"
        ? stored.showRightSidebar
        : DEFAULT_SETTINGS.showRightSidebar,
    pinnedWorkspaces: Array.isArray(stored.pinnedWorkspaces)
      ? stored.pinnedWorkspaces.filter((p): p is string => typeof p === "string")
      : DEFAULT_SETTINGS.pinnedWorkspaces,
    workspaceNames: isStringRecord(stored.workspaceNames)
      ? stored.workspaceNames
      : DEFAULT_SETTINGS.workspaceNames,
  };
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    Object.values(value).every((v) => typeof v === "string")
  );
}

/**
 * A module-level store, read through `useSyncExternalStore`.
 *
 * `localStorage` is an external system, and this is the hook React provides for
 * subscribing to one. The alternative — state plus an effect that reads storage
 * after mount — sets state during an effect, which cascades an extra render on
 * every mount and is what `react-hooks/set-state-in-effect` flags.
 *
 * Two properties fall out of doing it this way. The server snapshot is a
 * separate, stable value, so hydration cannot mismatch. And subscribing to
 * `storage` events means a change in one tab reaches the others, which a
 * per-component effect could not do at all.
 */
let cached: Settings | null = null;
const listeners = new Set<() => void>();

function readStore(): Settings {
  if (cached) return cached;
  try {
    cached = hydrate(window.localStorage.getItem(STORAGE_KEY));
  } catch {
    // Private browsing and storage-disabled profiles throw on access. The app
    // stays usable with defaults; only persistence is lost.
    cached = DEFAULT_SETTINGS;
  }
  return cached;
}

function writeStore(next: Settings) {
  cached = next;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // An unavailable store must not break the interaction that wrote to it.
  }
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  const onStorage = (event: StorageEvent) => {
    if (event.key !== null && event.key !== STORAGE_KEY) return;
    // Drop the cache so the next read re-parses what the other tab wrote.
    cached = null;
    for (const each of listeners) each();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("storage", onStorage);
  };
}

/**
 * Server snapshot.
 *
 * Must be referentially stable across calls, or React re-renders forever during
 * hydration. Returning `DEFAULT_SETTINGS` (a module constant) satisfies that;
 * building a fresh object here would not.
 */
function getServerSnapshot(): Settings {
  return DEFAULT_SETTINGS;
}

export function useSettings(): SettingsContextValue {
  const settings = useSyncExternalStore(subscribe, readStore, getServerSnapshot);

  const update = useCallback((patch: Partial<Settings>) => {
    writeStore({ ...readStore(), ...patch });
  }, []);

  const reset = useCallback(() => {
    try {
      window.localStorage.removeItem(STORAGE_KEY);
    } catch {
      /* nothing to clean up */
    }
    writeStore(DEFAULT_SETTINGS);
  }, []);

  return { settings, update, reset };
}

/**
 * Spacing scale for the conversation timeline, driven by the density setting.
 *
 * On the type scale rather than beside it: these were `13px` and `15px`, two
 * one-off sizes that existed only here and were within a pixel of scale steps.
 */
export function densityClasses(density: Density): { gap: string; text: string } {
  switch (density) {
    case "compact":
      return { gap: "space-y-1", text: "text-xs" };
    case "comfortable":
      return { gap: "space-y-4", text: "text-base" };
    default:
      return { gap: "space-y-2", text: "text-sm" };
  }
}
