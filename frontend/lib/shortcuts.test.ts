import { describe, expect, it } from "vitest";
import { comboFromEvent, formatCombo, makeCombo, pruneReserved, reservedReason } from "@/lib/shortcuts";
import { DEFAULT_SHORTCUTS } from "@/lib/settings";

/**
 * The combo grammar.
 *
 * Bindings are stored as strings and looked up in a map, so a combo must have
 * exactly one canonical spelling. Two spellings for one keystroke means a
 * user-customized shortcut silently stops working.
 */
function keyEvent(init: Partial<KeyboardEvent> & { key: string }): KeyboardEvent {
  // A plain object rather than a real KeyboardEvent: `comboFromEvent` reads five
  // properties, and this keeps the test runnable in a Node environment.
  return {
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...init,
  } as KeyboardEvent;
}

describe("comboFromEvent", () => {
  it("maps Cmd and Ctrl to the same platform-neutral Mod", () => {
    // Storing `Meta+k` would break the same user's bindings on a Linux machine.
    expect(comboFromEvent(keyEvent({ key: "k", metaKey: true }))).toBe("Mod+k");
    expect(comboFromEvent(keyEvent({ key: "k", ctrlKey: true }))).toBe("Mod+k");
  });

  it("orders modifiers canonically", () => {
    expect(
      comboFromEvent(keyEvent({ key: "n", metaKey: true, shiftKey: true, altKey: true })),
    ).toBe("Mod+Alt+Shift+n");
    // The same keystroke reported with Ctrl instead of Meta canonicalizes equally.
    expect(
      comboFromEvent(keyEvent({ key: "n", ctrlKey: true, altKey: true, shiftKey: true })),
    ).toBe("Mod+Alt+Shift+n");
  });

  it("lowercases single characters so a shifted binding is typable", () => {
    // With Shift held the browser reports `E`, so treating `Shift+E` and
    // `Shift+e` as different combos would make the binding impossible to press.
    expect(comboFromEvent(keyEvent({ key: "E", shiftKey: true }))).toBe("Shift+e");
    expect(comboFromEvent(keyEvent({ key: "e", shiftKey: true }))).toBe("Shift+e");
  });

  it("keeps named keys in their reported form", () => {
    expect(comboFromEvent(keyEvent({ key: "Escape" }))).toBe("Escape");
    expect(comboFromEvent(keyEvent({ key: "ArrowDown" }))).toBe("ArrowDown");
    expect(comboFromEvent(keyEvent({ key: "Enter", metaKey: true }))).toBe("Mod+Enter");
  });

  it("handles punctuation bindings", () => {
    expect(comboFromEvent(keyEvent({ key: ",", metaKey: true }))).toBe("Mod+,");
    expect(comboFromEvent(keyEvent({ key: ".", metaKey: true }))).toBe("Mod+.");
    expect(comboFromEvent(keyEvent({ key: "\\", metaKey: true }))).toBe("Mod+\\");
  });

  it("agrees with makeCombo, so the recorder and the dispatcher cannot diverge", () => {
    expect(makeCombo({ mod: true }, "k")).toBe(comboFromEvent(keyEvent({ key: "k", metaKey: true })));
    expect(makeCombo({ mod: true, shift: true }, "N")).toBe(
      comboFromEvent(keyEvent({ key: "N", metaKey: true, shiftKey: true })),
    );
    expect(makeCombo({}, "Escape")).toBe(comboFromEvent(keyEvent({ key: "Escape" })));
  });
});

describe("formatCombo", () => {
  it("uses macOS glyphs, concatenated", () => {
    expect(formatCombo("Mod+k", true)).toBe("⌘K");
    expect(formatCombo("Mod+Shift+n", true)).toBe("⌘⇧N");
    expect(formatCombo("Mod+Alt+Shift+n", true)).toBe("⌘⌥⇧N");
  });

  it("uses plus-joined names elsewhere", () => {
    expect(formatCombo("Mod+k", false)).toBe("Ctrl+K");
    expect(formatCombo("Mod+Shift+n", false)).toBe("Ctrl+Shift+N");
  });

  it("renders named keys as symbols where there is a conventional one", () => {
    expect(formatCombo("Escape", true)).toBe("Esc");
    expect(formatCombo("Mod+Enter", true)).toBe("⌘↵");
    expect(formatCombo("ArrowDown", false)).toBe("↓");
    // An unmapped named key falls through to its own name rather than vanishing.
    expect(formatCombo("F5", false)).toBe("F5");
  });

  it("round-trips every default-style combo", () => {
    for (const combo of ["Mod+k", "Mod+p", "Mod+,", "Mod+\\", "Shift+e"]) {
      expect(formatCombo(combo, true).length).toBeGreaterThan(0);
      expect(formatCombo(combo, false).length).toBeGreaterThan(0);
    }
  });
});

/*
 * `isTypingTarget` is not unit-tested here: it is an `instanceof HTMLElement`
 * check, which is always false under Node, so a passing assertion would prove
 * nothing. Its real behaviour — that `Shift+E` in the composer does not expand
 * the timeline — is covered in `e2e/workspace-shell.spec.ts`, in a browser.
 */

/**
 * The browser's reserved set.
 *
 * These pin the fix for a real defect: `Mod+N` and `Mod+Shift+N` shipped as the
 * defaults for "new session" and "Quick Start", and in a browser tab they open a
 * new window and an incognito window instead. `preventDefault` does not reach
 * them — Chrome has reserved the set since version 4 — so the only remedies are
 * to not bind them and to refuse them when a user tries.
 */
describe("reserved combos", () => {
  it("does not bind a reserved combo by default", () => {
    const reserved = Object.keys(DEFAULT_SHORTCUTS).filter((combo) => reservedReason(combo));
    expect(reserved).toEqual([]);
  });

  it("still binds every action to something", () => {
    // The fix moved two bindings. An action left unbound would be a shortcut
    // silently removed rather than relocated.
    const bound = new Set(Object.values(DEFAULT_SHORTCUTS));
    for (const action of [
      "search",
      "commandPalette",
      "browseWorkspaceHistory",
      "newSession",
      "quickStart",
      "toggleLeftSidebar",
      "toggleRightSidebar",
      "openSettings",
      "stopTurn",
      "collapseAll",
      "findInSession",
    ]) {
      expect(bound.has(action as never), `${action} has no default binding`).toBe(true);
    }
  });

  it("binds each default combo to exactly one action", () => {
    // Stored as combo -> action, so a duplicated combo cannot exist by
    // construction; a duplicated *action* would mean two combos race and the
    // panel's reverse lookup would show whichever came first.
    const actions = Object.values(DEFAULT_SHORTCUTS);
    expect(new Set(actions).size).toBe(actions.length);
  });

  it("explains what the browser does instead, not merely that it refuses", () => {
    expect(reservedReason("Mod+n")).toContain("new browser window");
    expect(reservedReason("Mod+Shift+n")).toContain("incognito");
    expect(reservedReason("Mod+t")).toContain("tab");
  });

  it("treats a bindable combo as bindable", () => {
    expect(reservedReason("Mod+k")).toBeNull();
    expect(reservedReason("Mod+Shift+o")).toBeNull();
    expect(reservedReason("Mod+j")).toBeNull();
  });

  /**
   * The migration. A settings write persists the whole merged map, so anyone who
   * had ever opened the shortcuts panel has `Mod+n` stored from when it was a
   * default — and it would keep opening a browser window while shadowing the
   * replacement binding in the panel's reverse lookup.
   */
  it("prunes a stored binding the browser will never deliver", () => {
    const stored = {
      "Mod+n": "newSession",
      "Mod+Shift+n": "quickStart",
      "Mod+k": "search",
    } as const;

    expect(pruneReserved(stored)).toEqual({ "Mod+k": "search" });
  });

  it("leaves a map with nothing reserved untouched", () => {
    expect(pruneReserved(DEFAULT_SHORTCUTS)).toEqual(DEFAULT_SHORTCUTS);
  });

  /** A combo is matched by its canonical spelling, so the table must use it. */
  it("keys the table by the same spelling comboFromEvent produces", () => {
    const combo = comboFromEvent(keyEvent({ key: "n", metaKey: true }));
    expect(reservedReason(combo)).toBeTruthy();

    const shifted = comboFromEvent(keyEvent({ key: "N", metaKey: true, shiftKey: true }));
    expect(reservedReason(shifted)).toBeTruthy();
  });
});
