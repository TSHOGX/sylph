"use client";

/**
 * The skip link, as a client component so its text can be translated.
 *
 * It is the first thing in the document and the first thing a keyboard user
 * reaches, so it is exactly the string that should not be stuck in one language —
 * and it is the one control whose *only* purpose is to be read aloud.
 *
 * A client component purely to reach `useT`: the locale lives in `localStorage`
 * beside theme and density, so the server cannot know it, and `app/layout.tsx` is a
 * server component. Everything else about the element is unchanged, including the
 * `sr-only` / `focus:not-sr-only` pair that keeps it invisible until focused.
 */

import { useT } from "@/lib/i18n";

export function SkipLink() {
  const t = useT();

  return (
    <a
      href="#main"
      className="sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4 focus:z-50 focus:rounded-lg focus:bg-accent focus:px-3 focus:py-2 focus:text-sm focus:text-accent-ink"
    >
      {t("app.skipToMain")}
    </a>
  );
}
