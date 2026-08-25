"use client";

/**
 * The identity section of settings: what Sylph appends to an agent's prompt, and
 * whether to append it at all.
 *
 * Its own file rather than a fifth function in `settings-panel.tsx`, because it is
 * the only section that fetches and renders a document. The panel is otherwise a
 * list of controls; folding a scrollable text preview with its own loading and
 * error states into it would make the largest file in the panel the one holding
 * the simplest toggles.
 *
 * # Why the text is shown at all
 *
 * A toggle governing whether `SOUL.md` is appended to every prompt is a request
 * for consent to something the user cannot otherwise see: the file lives in the
 * Sylph home directory, outside any surface the app otherwise offers. Asking
 * someone to choose between "Sylph's identity" and "the agent as it comes" without
 * showing what the first one says is asking them to pick between two unknowns.
 *
 * Collapsed by default, because the answer to "is identity on" is the toggle, and
 * the file is 3 KB of prose that would otherwise push every other control in this
 * section below the fold.
 */

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { formatBytes, formatRelative } from "@/lib/format";
import { useT } from "@/lib/i18n";
import { useSettings } from "@/lib/settings";
import { Field } from "@/components/settings-panel";

export function IdentitySection() {
  const { settings, update } = useSettings();
  const t = useT();
  const [showText, setShowText] = useState(false);

  // Not cached aggressively: SOUL.md is hand-edited between sessions, and a stale
  // copy shown beside a toggle claiming to control the live one is worse than a
  // second request. `staleTime: 0` with the panel's own mount as the trigger means
  // it is re-read each time the section is opened.
  const soul = useQuery({
    queryKey: ["soul"],
    queryFn: () => api.soul.get(),
    staleTime: 0,
  });

  const available = soul.data?.available ?? false;

  return (
    <div className="space-y-4">
      <Field label={t("settings.identity.inject.label")} hint={t("settings.identity.inject.hint")}>
        {/*
          Two buttons rather than a checkbox. The choice is between two named
          policies, and both names carry information — "use the agent as it comes"
          is not merely the absence of "append SOUL.md", it is what the session
          will be. An unchecked box would leave the user to infer the alternative.

          Both stay enabled when no SOUL.md exists, and the notice below explains
          why the choice has no effect right now. Disabling them would leave the
          stored preference invisible and unreadable, so a user who later adds a
          SOUL.md would not know what they had chosen.
        */}
        <div className="flex flex-wrap gap-1.5">
          {([true, false] as const).map((value) => (
            <button
              key={String(value)}
              type="button"
              onClick={() => update({ injectSoul: value })}
              aria-pressed={settings.injectSoul === value}
              className={`tap-target focus-ring rounded-md border px-2 py-1 text-xs ${
                settings.injectSoul === value
                  ? "border-accent bg-accent text-accent-ink"
                  : "border-line hover:bg-hover"
              }`}
            >
              {value ? t("settings.identity.inject.on") : t("settings.identity.inject.off")}
            </button>
          ))}
        </div>

        {!soul.isPending && !available && (
          <p className="rounded border border-warn-line bg-warn-bg px-2 py-1 text-2xs leading-relaxed text-ink-soft">
            {t("settings.identity.unavailable")}
          </p>
        )}

        <p className="text-2xs leading-relaxed text-ink-muted">
          {t("settings.identity.appliesTo")}
        </p>
      </Field>

      {available && (
        <Field label={t("settings.identity.preview.label")}>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={() => setShowText((shown) => !shown)}
              aria-expanded={showText}
              className="rounded border border-line-strong px-2 py-1 text-xs font-medium hover:bg-sunken focus-ring"
            >
              {showText
                ? t("settings.identity.preview.hide")
                : t("settings.identity.preview.show")}
            </button>
            {soul.data?.bytes !== undefined && (
              <span className="text-2xs text-ink-muted">
                {t("settings.identity.meta", {
                  bytes: formatBytes(soul.data.bytes),
                  when: formatRelative(soul.data.modifiedMs),
                })}
              </span>
            )}
          </div>

          {/* The path is shown unconditionally, not only when expanded: it is how a
              user finds the file to edit it, and it is short. `break-all` because
              an absolute path has no spaces to wrap at and would otherwise force
              the whole panel to scroll sideways. */}
          <p className="rounded border border-line bg-sunken px-2 py-1 font-mono text-2xs break-all text-ink-muted">
            {soul.data?.path}
          </p>

          {soul.data?.error && (
            <p role="alert" className="text-2xs text-danger">
              {t("settings.identity.preview.readFailed")} {soul.data.error}
            </p>
          )}

          {showText && soul.data?.content && (
            <>
              {soul.data.truncated && (
                <p className="text-2xs text-ink-muted">
                  {t("settings.identity.preview.truncated")}
                </p>
              )}
              {/*
                A `<pre>` rather than the markdown renderer. This is the text as
                the agent receives it — a prompt preamble, not a document — and
                rendering the markdown would hide the `#` and `---` that are part
                of what gets sent. `tabIndex` because a scrollable region has to be
                reachable by keyboard, or its content is unreadable without a
                mouse.
              */}
              <pre
                tabIndex={0}
                aria-label={t("settings.identity.preview.label")}
                className="max-h-72 overflow-auto overscroll-contain rounded border border-line bg-sunken p-2 font-mono text-2xs leading-relaxed whitespace-pre-wrap text-ink-soft focus-ring"
              >
                {soul.data.content}
              </pre>
            </>
          )}
        </Field>
      )}
    </div>
  );
}
