"use client";

/**
 * Translation hooks.
 *
 * The locale is a settings field, so it is read through the same external store
 * every other preference uses — no provider, no context, and a language change
 * re-renders exactly the components that call `useT`.
 *
 * # Why a hand-rolled catalogue rather than a framework
 *
 * `next-intl` and `next-i18next` are both built around locale-in-the-URL: a
 * `[locale]` route segment, middleware that negotiates and redirects, and a
 * per-request catalogue load on the server. None of that applies here. This app
 * is one user's loopback tool, its language sits beside theme and density in
 * `localStorage`, and every string lives in a client component. Adopting the
 * routing model would mean restructuring `app/` around a segment that exists only
 * to carry a value storage already holds.
 *
 * What a framework would genuinely buy — ICU MessageFormat, per-locale code
 * splitting, plural and date rules — is either already provided by the platform
 * (`Intl.*`, used in `lib/format.ts`) or not worth its weight at this size: both
 * catalogues together are a few kilobytes gzipped, less than the machinery needed
 * to load one lazily. Revisit if a third locale arrives with translators who need
 * a standard file format, or if the catalogue passes roughly a thousand keys.
 */

import { useCallback } from "react";
import { useSettings } from "@/lib/settings";
import { translate, type Locale, type MessageKey, type Translate } from "./locale";

/**
 * The translation function for the active locale.
 *
 * Memoized on the locale alone, so a component that reads `t` and nothing else
 * re-renders only when the language changes — and so `t` is stable enough to sit
 * in another hook's dependency list without re-running it every render.
 */
export function useT(): Translate {
  const locale = useSettings().settings.locale;

  return useCallback(
    (key: MessageKey, params?: Record<string, string | number>) => translate(locale, key, params),
    [locale],
  );
}

/** The active locale, for `Intl` calls and `lang` attributes. */
export function useLocale(): Locale {
  return useSettings().settings.locale;
}

export {
  applyLocale,
  DEFAULT_LOCALE,
  format,
  formatParts,
  isLocale,
  LOCALE_NAMES,
  LOCALE_TAGS,
  LOCALES,
  translate,
} from "./locale";
export type { Dictionary, Locale, MessageKey, Translate } from "./locale";
