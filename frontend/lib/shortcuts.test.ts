import { describe, expect, it } from "vitest";
import { comboFromEvent, formatCombo, makeCombo } from "@/lib/shortcuts";

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
