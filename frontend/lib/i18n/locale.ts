/**
 * Locale primitives: the set of languages, their tags, and message lookup.
 *
 * Split from `./index.ts` to break an import cycle, and the split is load-bearing
 * rather than cosmetic. `lib/settings` has to know what a `Locale` is in order to
 * store and validate one, while `useT` has to read `lib/settings` to know which
 * locale is active. Everything here is free of React and of the settings store,
 * so the dependency runs one way: settings and the hook both import this, and
 * this imports neither.
 *
 * It is also the half worth unit-testing — placeholder substitution and fallback
 * are pure functions, testable without rendering anything.
 */

import { en, type Dictionary, type MessageKey } from "./messages";
import { zh } from "./zh";

/**
 * Locales the UI ships.
 *
 * `en` is first because it is the default and the fallback. Adding one means a
 * `Dictionary`-typed catalogue beside `zh.ts` and an entry in each record below;
 * nothing else in the app enumerates locales.
 */
export const LOCALES = ["en", "zh"] as const;

export type Locale = (typeof LOCALES)[number];

export const DEFAULT_LOCALE: Locale = "en";

/**
 * Each locale's own name, in its own language.
 *
 * Endonyms rather than translated names: a user looking for Chinese scans for
 * 中文, and someone who has switched to a language they cannot read needs a way
 * back — which they only have if the entry they want is written in the language
 * they know. This is why the picker's own labels are not translated.
 */
export const LOCALE_NAMES: Record<Locale, string> = {
  en: "English",
  zh: "中文",
};

/**
 * BCP 47 tags, for `<html lang>` and `Intl`.
 *
 * Separate from the dictionary keys because the two answer different questions:
 * `zh` is a fine object key but an underspecified language tag, while `zh-Hans`
 * is what tells a screen reader which pronunciation to use.
 */
export const LOCALE_TAGS: Record<Locale, string> = {
  en: "en",
  zh: "zh-Hans",
};

const DICTIONARIES: Record<Locale, Dictionary> = { en, zh };

export function isLocale(value: unknown): value is Locale {
  return typeof value === "string" && (LOCALES as readonly string[]).includes(value);
}

/**
 * Substitute `{name}` placeholders.
 *
 * Deliberately not a template-literal or `eval`-based scheme: these values are
 * interpolated into text that reaches the DOM, and a substitution that could
 * execute would turn a translation file into a script. A single regex pass also
 * leaves an unmatched placeholder verbatim, which is visible in the UI and
 * therefore gets fixed — dropping it silently would hide the bug.
 */
export function format(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in params ? String(params[name]) : whole,
  );
}

/**
 * A bound translator: a key and optional parameters in, a string out.
 *
 * Declared here rather than beside `useT` so a non-React caller can take one as a
 * parameter — `describeWindow` and `describeBlocked` do, which is what keeps their
 * plural rules in the catalogue while leaving them pure and unit-testable.
 */
export type Translate = (key: MessageKey, params?: Record<string, string | number>) => string;

/**
 * Split a translated string around one placeholder.
 *
 * For the case a plain string cannot serve: a sentence whose interpolated value
 * is a *path* or a *filename* and has to render in a mono face, inside a `<span>`
 * of its own. Concatenating a prefix and a suffix key would be the usual answer
 * and it is the wrong one here — English puts the value first ("Inside src/") and
 * Chinese wraps it ("在 src/ 内"), so the two halves are not independently
 * translatable and a translator cannot see the whole sentence.
 *
 * Returns the text before and after the slot, each with any remaining
 * placeholders substituted. An absent slot yields the whole string as the prefix,
 * which renders the sentence intact minus the value — visibly wrong rather than
 * silently empty.
 */
export function formatParts(
  template: string,
  slot: string,
  params?: Record<string, string | number>,
): [string, string] {
  const token = `{${slot}}`;
  const at = template.indexOf(token);
  if (at === -1) return [format(template, params), ""];
  return [
    format(template.slice(0, at), params),
    format(template.slice(at + token.length), params),
  ];
}

/**
 * Translate one key against an explicit locale.
 *
 * Falls back to English for a key the dictionary lacks. That cannot happen for a
 * locale in this repo, since `Dictionary` requires the full key set, but it is
 * the right behaviour for the case that can: a catalogue that failed to load
 * leaves the UI in English rather than showing raw keys.
 */
export function translate(
  locale: Locale,
  key: MessageKey,
  params?: Record<string, string | number>,
): string {
  const dictionary = DICTIONARIES[locale] ?? en;
  return format(dictionary[key] ?? en[key], params);
}

/**
 * Write the locale onto `<html lang>`.
 *
 * The attribute is not decoration. A screen reader picks its voice and
 * pronunciation from it, so a Chinese UI still labelled `lang="en"` is read aloud
 * as English attempting Chinese. It is applied client-side because the locale
 * lives in `localStorage`, which the server cannot see; the document ships
 * `lang="en"`, which is both the default and correct for the common case.
 */
export function applyLocale(locale: Locale): void {
  if (typeof document === "undefined") return;
  document.documentElement.lang = LOCALE_TAGS[locale];
}

export type { MessageKey, Dictionary } from "./messages";
