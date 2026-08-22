/**
 * Applying the colour theme to the document.
 *
 * Two halves, and both are necessary:
 *
 * 1. **Before first paint.** `THEME_BOOTSTRAP` is an inline, blocking script in
 *    the document head. Without it a dark-mode user sees a white page for the
 *    duration of hydration — which on a cold load is long enough to read as a
 *    flash of the wrong app. It cannot be a React effect for the same reason: an
 *    effect runs after paint, by definition too late.
 * 2. **After a preference change.** `applyTheme` is called by the settings store
 *    subscriber so switching Light/Dark/System repaints immediately.
 *
 * The two must agree, so both resolve `system` the same way and both write the
 * same attribute. The script is generated from the same `STORAGE_KEY` the store
 * uses, so the pre-paint read and the in-app read cannot drift.
 */

import { STORAGE_KEY, type Theme } from "@/lib/settings";

const DARK_QUERY = "(prefers-color-scheme: dark)";

/** The theme actually in effect: `system` resolved against the OS preference. */
export function resolveTheme(theme: Theme): "light" | "dark" {
  if (theme !== "system") return theme;
  if (typeof window === "undefined" || !window.matchMedia) return "light";
  return window.matchMedia(DARK_QUERY).matches ? "dark" : "light";
}

/**
 * Write the resolved theme onto `<html>`.
 *
 * `<html>` specifically, not `<body>`: the token block is declared on `:root`,
 * and a custom property containing `color-mix(...)` substitutes on the element
 * that carries the declaration. Setting the attribute lower would leave every
 * derived `*-bg` and `*-line` mixed against the *light* canvas while the base
 * hues had gone dark — subtly wrong tints rather than an obvious failure.
 */
export function applyTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = resolveTheme(theme);
}

/**
 * Keep a `system` preference live.
 *
 * Returns an unsubscriber. A no-op for an explicit Light or Dark choice, which is
 * what makes an explicit choice stick even when the OS flips at sunset.
 */
export function watchSystemTheme(theme: Theme, onChange: () => void): () => void {
  if (theme !== "system" || typeof window === "undefined" || !window.matchMedia) {
    return () => {};
  }
  const query = window.matchMedia(DARK_QUERY);
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

/**
 * The pre-paint script, as source text.
 *
 * Deliberately tiny and defensive: it runs before anything else on the page, so
 * an exception here would be an exception before the app exists. A `try` around
 * the storage read covers private-browsing profiles, where `localStorage` access
 * throws rather than returning null, and the fallback is the OS preference —
 * which is the right answer for a first-ever visit anyway.
 *
 * It also sets `color-scheme` on the root's inline style, so the browser paints
 * the canvas behind the page — the area exposed by overscroll, and the scrollbar
 * gutter — in the right shade during the same paint. The stylesheet sets this
 * too; the duplication buys the very first frame, before CSS has applied.
 */
export const THEME_BOOTSTRAP = `try{var s=localStorage.getItem(${JSON.stringify(
  STORAGE_KEY,
)});var t=s?JSON.parse(s).theme:null;if(t!=="light"&&t!=="dark"){t=matchMedia("${DARK_QUERY}").matches?"dark":"light"}var r=document.documentElement;r.dataset.theme=t;r.style.colorScheme=t}catch(e){var r2=document.documentElement;r2.dataset.theme="light";r2.style.colorScheme="light"}`;
