/**
 * Keyboard shortcut grammar, matching, and the browser's reserved set.
 *
 * A combo is written `Mod+Shift+k`: modifiers in a fixed order, then a key.
 * `Mod` is Cmd on macOS and Ctrl elsewhere, which is why bindings are stored
 * platform-neutral and resolved at match time — a stored `Meta+k` would silently
 * stop working for the same user on a Linux machine.
 *
 * Ordering is canonical (`Mod`, `Alt`, `Shift`, key) so a binding has exactly
 * one spelling and the settings panel can use the string as a map key.
 *
 * Everything here is pure: no React, and no dependency on the settings store.
 * That is what lets `lib/settings` prune reserved bindings while hydrating,
 * without the import cycle that would come from reaching into `lib/shortcuts` —
 * whose hooks read the store. The hooks re-export this module, so call sites
 * import either and see one API.
 */


export type Combo = string;

const MODIFIER_ORDER = ["Mod", "Alt", "Shift"] as const;

/**
 * Combos a browser keeps for itself, which therefore cannot be bound here.
 *
 * These are not "combos we would rather avoid". A page cannot intercept them at
 * all: Chrome has reserved this set since version 4 (2010), and the others match
 * it. The keydown may or may not reach the document, but `preventDefault()` on it
 * does nothing, so the browser acts *and* the app acts. That is how `Cmd+N` came
 * to open a new browser window while also opening the new-session dialog behind
 * it, and how `Cmd+Shift+N` opened an incognito window — the two bugs that
 * produced this table.
 *
 * The values are what each combo does, phrased for a user reading it in the
 * settings panel rather than as an internal note.
 *
 * # Scope, stated honestly
 *
 * This covers the window-and-tab family, which is reserved on every desktop
 * browser and is the part that has actually bitten. It is not a complete
 * catalogue of every unavailable combination anywhere:
 *
 * - Alt-prefixed combos overlap OS-level and menu accelerators unpredictably per
 *   platform, so they are avoided in defaults rather than enumerated.
 * - An installed PWA in standalone mode *does* receive some of these, because it
 *   has no tab strip to act on. They stay refused regardless: the same
 *   preference is used in a browser tab, and a binding that works only when
 *   launched from the dock is worse than one that never works.
 * - `Cmd+Q` and `Cmd+W` are absent deliberately — they quit and close, so a page
 *   that captured them would trap the user. Nothing here should offer them.
 *
 * Verified rather than assumed: Chrome's reservation of Ctrl/Cmd+N, +T, +W and
 * their shifted forms is long-standing documented behaviour, not something
 * `preventDefault` can reach.
 */
export const RESERVED_COMBOS: Record<Combo, string> = {
  "Mod+n": "opens a new browser window",
  "Mod+Shift+n": "opens a new incognito window",
  "Mod+t": "opens a new browser tab",
  "Mod+Shift+t": "reopens the last closed tab",
  "Mod+w": "closes the tab",
  "Mod+Shift+w": "closes the window",
  "Mod+q": "quits the browser",
};

/**
 * Why a combo cannot be bound, or `null` when it can.
 *
 * Returning the reason rather than a boolean is what lets the settings panel say
 * *what* the browser will do instead, which is the difference between a refusal a
 * user can act on and one that reads as an arbitrary restriction.
 */
export function reservedReason(combo: Combo): string | null {
  return RESERVED_COMBOS[combo] ?? null;
}

/**
 * Drop bindings the browser will never deliver.
 *
 * Applied when settings are hydrated, so a preference stored before this table
 * existed does not keep a binding that silently does nothing. Without it, a user
 * who had ever opened the shortcuts panel would keep `Mod+n` — the settings write
 * persists the whole merged map, not just the row that changed — and would go on
 * seeing a new browser window with the app's dialog behind it.
 */
export function pruneReserved<T>(bindings: Record<Combo, T>): Record<Combo, T> {
  const kept: Record<Combo, T> = {};
  for (const [combo, action] of Object.entries(bindings)) {
    if (!(combo in RESERVED_COMBOS)) kept[combo] = action;
  }
  return kept;
}

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
