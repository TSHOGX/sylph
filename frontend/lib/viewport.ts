"use client";

/**
 * Is this a phone-sized viewport?
 *
 * # Why any JavaScript is involved at all
 *
 * Layout is CSS: the shell's panes, widths, and drawer transforms are Tailwind
 * breakpoints, so nothing here decides how the app *looks*. What CSS cannot
 * express is *behaviour* that differs by form factor:
 *
 * - A drawer must trap focus and close on Escape when it is an overlay, and must
 *   not when it is an in-flow column. A focus trap is not a style.
 * - Tapping a session in the drawer should dismiss it; clicking one in a
 *   permanent sidebar should not.
 * - Opening a file on a phone goes full-screen instead of into a 320px pane, so
 *   which component renders differs, not merely its width.
 *
 * Each of those is a different component tree, not a different rule.
 *
 * # Why `useSyncExternalStore`
 *
 * `matchMedia` is an external system, and this is the hook React provides for
 * subscribing to one. The alternative — `useState` plus an effect that measures
 * after mount — cascades a second render on every mount and is what
 * `react-hooks/set-state-in-effect` flags. It also mismatches during hydration,
 * because the first render has no window to measure.
 *
 * The server snapshot is `false` (desktop). That is the safe default: a desktop
 * layout briefly rendered on a phone settles on the first client render, whereas
 * defaulting to mobile would flash a hamburger on every desktop load.
 *
 * The breakpoint is 768px, matching Tailwind's `md`, so the JS boundary and the
 * CSS boundary are the same number. They must be: a component that renders a
 * focus trap while CSS still lays the pane out in flow would trap focus inside a
 * visible sidebar.
 */

import { useSyncExternalStore } from "react";

/** Tailwind's `md`. Keep in sync with the `md:` classes in the shell. */
export const MOBILE_MAX_WIDTH = 767;

const QUERY = `(max-width: ${MOBILE_MAX_WIDTH}px)`;

/**
 * One `MediaQueryList` for every consumer, created lazily.
 *
 * Module-level rather than per-hook: `matchMedia` allocates a live object with
 * its own listener list, and the shell, the composer, and the artifacts pane all
 * ask the same question. One object also guarantees they cannot answer it
 * differently mid-render.
 */
let query: MediaQueryList | null = null;

function getQuery(): MediaQueryList | null {
  if (typeof window === "undefined" || !window.matchMedia) return null;
  query ??= window.matchMedia(QUERY);
  return query;
}

function subscribe(onChange: () => void): () => void {
  const list = getQuery();
  if (!list) return () => {};
  list.addEventListener("change", onChange);
  return () => list.removeEventListener("change", onChange);
}

function getSnapshot(): boolean {
  return getQuery()?.matches ?? false;
}

function getServerSnapshot(): boolean {
  return false;
}

/** True on a phone-sized viewport. See the module comment for what this gates. */
export function useIsMobile(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
