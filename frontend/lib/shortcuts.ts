/**
 * Keyboard shortcut grammar and matching.
 *
 * A combo is written `Mod+Shift+k`: modifiers in a fixed order, then a key.
 * `Mod` is Cmd on macOS and Ctrl elsewhere, which is why bindings are stored
 * platform-neutral and resolved at match time — a stored `Meta+k` would silently
 * stop working for the same user on a Linux machine.
 *
 * Ordering is canonical (`Mod`, `Alt`, `Shift`, key) so a binding has exactly
 * one spelling and the settings panel can use the string as a map key.
 */

import { useSyncExternalStore } from "react";

export type Combo = string;

const MODIFIER_ORDER = ["Mod", "Alt", "Shift"] as const;

/**
 * Canonical combo for a keyboard event.
 *
 * `event.key` is used rather than `event.code` so a binding follows the
 * character the user actually types on their layout. The single-character case
 * is lowercased so `Shift+E` and `Shift+e` are one binding: with Shift held,
 * `key` is already the uppercase form, and treating those as distinct would make
 * every shifted binding impossible to type.
 */
export function comboFromEvent(event: KeyboardEvent): Combo {
  const parts: string[] = [];
  // Mac uses Meta for Cmd; every other platform uses Ctrl. Reading both means a
  // Ctrl+K on a Mac keyboard also matches, which is harmless and helps users
  // coming from other platforms.
  if (event.metaKey || event.ctrlKey) parts.push("Mod");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");

  const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
  parts.push(key);
  return parts.join("+");
}

/** Render a combo for display, using the platform's own symbols. */
export function formatCombo(combo: Combo, isMac: boolean): string {
  const parts = combo.split("+");
  const key = parts.pop() ?? "";
  const rendered = parts.map((part) => {
    if (part === "Mod") return isMac ? "⌘" : "Ctrl";
    if (part === "Alt") return isMac ? "⌥" : "Alt";
    if (part === "Shift") return isMac ? "⇧" : "Shift";
    return part;
  });

  const label =
    key.length === 1
      ? key.toUpperCase()
      : { ArrowUp: "↑", ArrowDown: "↓", Escape: "Esc", Enter: "↵" }[key] ?? key;

  // macOS convention concatenates modifier symbols; elsewhere they are joined
  // with `+`, which is what a Windows or Linux user reads as a shortcut.
  return isMac ? [...rendered, label].join("") : [...rendered, label].join("+");
}

/** Build a canonical combo from parts, for tests and the shortcut recorder. */
export function makeCombo(
  modifiers: { mod?: boolean; alt?: boolean; shift?: boolean },
  key: string,
): Combo {
  const parts: string[] = [];
  for (const name of MODIFIER_ORDER) {
    if (name === "Mod" && modifiers.mod) parts.push("Mod");
    if (name === "Alt" && modifiers.alt) parts.push("Alt");
    if (name === "Shift" && modifiers.shift) parts.push("Shift");
  }
  parts.push(key.length === 1 ? key.toLowerCase() : key);
  return parts.join("+");
}

export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  // `navigator.platform` is deprecated but is the only property that reliably
  // distinguishes macOS in every current browser; `userAgentData.platform` is
  // Chromium-only. The value is used purely to pick a glyph.
  return /Mac|iPhone|iPad/i.test(navigator.platform || navigator.userAgent);
}

/**
 * Platform detection as an external-store read.
 *
 * The platform is a property of the environment, not React state, and reading it
 * in an effect that then calls `setState` costs a second render on every mount of
 * every component that shows a shortcut hint. `useSyncExternalStore` with a
 * never-firing subscribe expresses "constant, but unavailable on the server":
 * the server snapshot is `false`, so shortcut hints render in their non-Mac form
 * during prerender and settle on hydration without a mismatch.
 */
const noopSubscribe = () => () => {};

export function useIsMac(): boolean {
  return useSyncExternalStore(noopSubscribe, isMacPlatform, () => false);
}

/**
 * True when a key event originated in a text-entry context.
 *
 * A shortcut must not fire while the user is typing a message: `Shift+E` belongs
 * to the composer, not to "expand all". Combos carrying `Mod` are exempt,
 * because `Cmd+K` is expected to work from anywhere including the composer.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    target.isContentEditable ||
    // Radix-style widgets and our own dialogs mark their own text surfaces.
    target.getAttribute("role") === "textbox"
  );
}
