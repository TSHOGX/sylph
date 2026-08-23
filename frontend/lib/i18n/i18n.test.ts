import { describe, expect, it } from "vitest";
import { en, type MessageKey } from "./messages";
import { zh } from "./zh";
import {
  format,
  formatParts,
  isLocale,
  LOCALES,
  LOCALE_NAMES,
  LOCALE_TAGS,
  translate,
} from "./locale";

/**
 * The catalogue's own invariants.
 *
 * The compiler already requires a translation for every key — `Dictionary` is a
 * total record. What it cannot check is whether the two strings *say the same
 * thing structurally*: a translation that drops a `{combo}` placeholder compiles
 * fine and renders a sentence with a hole in it. These tests cover that gap.
 */
describe("the message catalogue", () => {
  it("translates every English key", () => {
    const missing = (Object.keys(en) as MessageKey[]).filter((key) => !zh[key]);
    expect(missing).toEqual([]);
  });

  it("has no translation for a key English does not define", () => {
    // A stale key is dead weight that reads as coverage. The compiler catches
    // this for `zh`, which is typed; the assertion documents the rule and guards
    // a future dictionary loaded less strictly.
    const extra = Object.keys(zh).filter((key) => !(key in en));
    expect(extra).toEqual([]);
  });

  /**
   * The failure this exists for: `settings.identity.meta` interpolates `{bytes}`
   * and `{when}`, and a translation that spells one of them differently renders
   * the placeholder verbatim to the user.
   */
  it("uses the same placeholders in every language", () => {
    const placeholders = (text: string) =>
      [...text.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();

    for (const key of Object.keys(en) as MessageKey[]) {
      expect(placeholders(zh[key]), `placeholders differ for ${key}`).toEqual(
        placeholders(en[key]),
      );
    }
  });

  it("declares a name and a language tag for every locale", () => {
    for (const locale of LOCALES) {
      expect(LOCALE_NAMES[locale]).toBeTruthy();
      expect(LOCALE_TAGS[locale]).toBeTruthy();
    }
  });

  /** Endonyms, not translated names: the reason is in `LOCALE_NAMES`. */
  it("names each locale in its own language", () => {
    expect(LOCALE_NAMES.en).toBe("English");
    expect(LOCALE_NAMES.zh).toBe("中文");
  });

  it("gives Chinese a script subtag, which a screen reader needs", () => {
    expect(LOCALE_TAGS.zh).toBe("zh-Hans");
  });
});

describe("placeholder substitution", () => {
  it("substitutes named parameters", () => {
    expect(format("{a} and {b}", { a: "one", b: "two" })).toBe("one and two");
  });

  it("substitutes numbers without the caller stringifying them", () => {
    expect(format("{n} runners", { n: 4 })).toBe("4 runners");
  });

  it("substitutes every occurrence of a repeated placeholder", () => {
    expect(format("{x}-{x}", { x: "a" })).toBe("a-a");
  });

  /**
   * Left verbatim rather than blanked. An unmatched placeholder is a bug in the
   * calling code, and a visible `{combo}` is what gets it reported; silently
   * emitting an empty string would produce a subtly broken sentence instead.
   */
  it("leaves an unsupplied placeholder in place so the bug is visible", () => {
    expect(format("hello {name}", {})).toBe("hello {name}");
  });

  it("returns the template unchanged when no parameters are given", () => {
    expect(format("no placeholders here")).toBe("no placeholders here");
  });

  /**
   * A value that looks like a placeholder must not be rescanned. Otherwise a
   * parameter could inject a substitution, which is the same class of problem as
   * template injection.
   */
  it("does not re-substitute inside a substituted value", () => {
    expect(format("{a}", { a: "{b}", b: "leaked" })).toBe("{b}");
  });
});

describe("formatParts", () => {
  it("splits around the slot, keeping both sides", () => {
    expect(formatParts("Inside {name}", "name")).toEqual(["Inside ", ""]);
  });

  /**
   * The reason this exists rather than a prefix key and a suffix key: Chinese
   * wraps the value where English precedes it, so both halves have to come from
   * one translated sentence.
   */
  it("keeps text on both sides when the language wraps the value", () => {
    expect(formatParts("在 {name} 内", "name")).toEqual(["在 ", " 内"]);
  });

  it("substitutes the other placeholders in each half", () => {
    expect(formatParts("{a} {name} {b}", "name", { a: "one", b: "two" })).toEqual([
      "one ",
      " two",
    ]);
  });

  /**
   * A sentence rendered without its value is visibly wrong, which gets it fixed.
   * Returning two empty strings would drop the sentence and hide the mistake.
   */
  it("returns the whole string when the slot is absent", () => {
    expect(formatParts("no slot here", "name")).toEqual(["no slot here", ""]);
  });

  it("splits on the first occurrence only", () => {
    expect(formatParts("{x} and {x}", "x")).toEqual(["", " and {x}"]);
  });
});

/**
 * Every key the catalogue splits with `formatParts` must actually carry the slot
 * in both languages. A translation that drops `{name}` type-checks and passes the
 * placeholder-agreement test only because that test compares the two catalogues
 * to each other — so if both were wrong together, nothing would catch it.
 */
describe("keys rendered around a value", () => {
  const SPLIT_KEYS: MessageKey[] = [
    "files.mkdir.inside",
    "files.delete.file",
    "files.delete.folder",
    "files.overwrite.body",
  ];

  it("carries the {name} slot in every locale", () => {
    for (const locale of LOCALES) {
      for (const key of SPLIT_KEYS) {
        expect(translate(locale, key), `${locale} ${key}`).toContain("{name}");
      }
    }
  });
});

describe("locale resolution", () => {
  it("recognizes the locales the app ships", () => {
    expect(isLocale("en")).toBe(true);
    expect(isLocale("zh")).toBe(true);
  });

  it("rejects anything else, including a plausible tag it does not ship", () => {
    expect(isLocale("zh-Hans")).toBe(false);
    expect(isLocale("fr")).toBe(false);
    expect(isLocale(undefined)).toBe(false);
    expect(isLocale(42)).toBe(false);
  });

  it("translates against the requested locale", () => {
    expect(translate("en", "action.close")).toBe("Close");
    expect(translate("zh", "action.close")).toBe("关闭");
  });

  it("interpolates while translating", () => {
    expect(translate("en", "settings.shortcuts.reserved", { combo: "⌘N" })).toContain("⌘N");
    expect(translate("zh", "settings.shortcuts.reserved", { combo: "⌘N" })).toContain("⌘N");
  });

  /**
   * The fallback exists for a dictionary that failed to load, not as licence to
   * skip keys. English text is a degraded UI; a raw key is a broken one.
   */
  it("falls back to English rather than showing a raw key", () => {
    // @ts-expect-error — an unknown locale is exactly the case being covered.
    expect(translate("de", "action.close")).toBe("Close");
  });
});
