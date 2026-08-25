"use client";

/**
 * `Cmd+P`: run an action without reaching for the mouse.
 *
 * Every entry dispatches through `runAction`, the same path the keyboard
 * shortcuts use. That is deliberate: a palette that reimplements what a shortcut
 * does drifts from it, and then the two disagree about what "Stop turn" means.
 *
 * Commands are grouped for scanning when the query is empty, and flattened into
 * one ranked list once the user types — a grouped fuzzy list forces the eye to
 * search several places for the best match.
 */

import { useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useSettings, type ShortcutAction } from "@/lib/settings";
import { formatCombo, useIsMac } from "@/lib/shortcuts";
import { fuzzyRank } from "@/lib/fuzzy";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useT } from "@/lib/i18n";
import { Overlay, scrollActiveIntoView, useListNavigation } from "@/components/overlay";
import { Kbd } from "@/components/ui/button";
import { CommandIcon } from "@/components/icons";

interface Command {
  id: string;
  label: string;
  group: string;
  run: () => void;
  /** Action this mirrors, so its bound shortcut can be shown alongside. */
  action?: ShortcutAction;
  /** Extra words to match on that are not in the label. */
  keywords?: string;
}

/** Recently used command ids, most recent first. */
const RECENT_KEY = "sylph-web.recent-commands.v1";
const RECENT_LIMIT = 5;

/** Recents from storage, tolerating anything that is not a string array. */
function readRecent(): string[] {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
  } catch {
    // Unavailable or corrupt storage costs recency ordering, nothing else.
    return [];
  }
}

export function CommandPalette() {
  const t = useT();
  const router = useRouter();
  const queryClient = useQueryClient();
  const { closeOverlay, runAction, leftOpen, rightOpen } = useWorkspaceUI();
  const { settings, update } = useSettings();

  // Query and cursor move together: every query change resets the cursor, and
  // holding them in one state object makes that a single update rather than an
  // effect that reacts to the query and sets more state.
  const [{ query, active }, setPicker] = useState({ query: "", active: 0 });
  const isMac = useIsMac();
  const listRef = useRef<HTMLDivElement>(null);

  // Read once, at first render, rather than in an effect: the palette is mounted
  // by the keypress that opens it, so there is no server render to mismatch, and
  // an effect would paint the unordered list for one frame first.
  const [recent, setRecent] = useState<string[]>(readRecent);

  const commands = useMemo<Command[]>(
    () => [
      {
        id: "new-session",
        label: t("palette.cmd.newSession"),
        group: t("palette.group.session"),
        action: "newSession",
        run: () => runAction("newSession"),
      },
      {
        id: "quick-start",
        // No directory in the label. It is configurable (`paths.quick_start_dir`),
        // so naming one here would be wrong for anyone who set it — and the label
        // is the wrong place to learn a path in any case; Settings → Configuration
        // reports the resolved one.
        label: t("palette.cmd.quickStart"),
        group: t("palette.group.session"),
        action: "quickStart",
        keywords: "default scratch quick",
        run: () => runAction("quickStart"),
      },
      {
        id: "stop-turn",
        label: t("palette.cmd.stopTurn"),
        group: t("palette.group.session"),
        action: "stopTurn",
        keywords: "cancel interrupt",
        run: () => runAction("stopTurn"),
      },
      {
        id: "search",
        label: t("palette.cmd.search"),
        group: t("palette.group.navigation"),
        action: "search",
        run: () => runAction("search"),
      },
      {
        id: "browse-history",
        label: t("palette.cmd.browseHistory"),
        group: t("palette.group.navigation"),
        action: "browseWorkspaceHistory",
        keywords: "past previous archive tracedb directory workdir",
        run: () => runAction("browseWorkspaceHistory"),
      },
      {
        id: "start-screen",
        label: t("palette.cmd.startScreen"),
        group: t("palette.group.navigation"),
        keywords: "home welcome recent start new session",
        run: () => {
          closeOverlay();
          router.push("/");
        },
      },
      {
        id: "archive",
        label: t("palette.cmd.archive"),
        group: t("palette.group.navigation"),
        keywords: "tracedb history import",
        run: () => {
          closeOverlay();
          router.push("/search");
        },
      },
      {
        id: "toggle-left",
        label: t(leftOpen ? "palette.cmd.hideLeft" : "palette.cmd.showLeft"),
        group: t("palette.group.view"),
        action: "toggleLeftSidebar",
        run: () => runAction("toggleLeftSidebar"),
      },
      {
        id: "toggle-right",
        label: t(rightOpen ? "palette.cmd.hideRight" : "palette.cmd.showRight"),
        group: t("palette.group.view"),
        action: "toggleRightSidebar",
        keywords: "files preview",
        run: () => runAction("toggleRightSidebar"),
      },
      {
        id: "expand-all",
        label: t("palette.cmd.expandAll"),
        group: t("palette.group.view"),
        action: "expandAll",
        run: () => runAction("expandAll"),
      },
      {
        id: "collapse-all",
        label: t("palette.cmd.collapseAll"),
        group: t("palette.group.view"),
        action: "collapseAll",
        run: () => runAction("collapseAll"),
      },
      {
        id: "toggle-thinking",
        label: t(
          settings.expandThinking
            ? "palette.cmd.collapseThinking"
            : "palette.cmd.expandThinking",
        ),
        group: t("palette.group.view"),
        keywords: "reasoning preference",
        run: () => {
          update({ expandThinking: !settings.expandThinking });
          closeOverlay();
        },
      },
      {
        id: "toggle-tool-output",
        label: t(
          settings.expandToolOutput
            ? "palette.cmd.collapseToolOutput"
            : "palette.cmd.expandToolOutput",
        ),
        group: t("palette.group.view"),
        run: () => {
          update({ expandToolOutput: !settings.expandToolOutput });
          closeOverlay();
        },
      },
      {
        id: "density",
        // The density *value* is translated too, read from the same keys the settings
        // panel renders — interpolating the raw enum would put an English word
        // inside a Chinese sentence.
        label: t("palette.cmd.density", {
          density: t(`settings.density.${settings.density}`),
        }),
        group: t("palette.group.view"),
        keywords: "spacing compact comfortable",
        run: () => {
          const order = ["compact", "normal", "comfortable"] as const;
          const next = order[(order.indexOf(settings.density) + 1) % order.length];
          update({ density: next });
          closeOverlay();
        },
      },
      {
        id: "settings",
        label: t("palette.cmd.settings"),
        group: t("palette.group.settings"),
        action: "openSettings",
        keywords: "preferences configuration",
        run: () => runAction("openSettings"),
      },
      {
        id: "refresh-agents",
        label: t("palette.cmd.refreshAgents"),
        group: t("palette.group.settings"),
        keywords: "discover describe",
        run: () => {
          // Re-runs `--describe` discovery on the backend, then seeds the cache
          // with the result so the create dialog sees new runners immediately.
          void queryClient
            .fetchQuery({ queryKey: ["agents"], queryFn: () => api.agents.refresh() })
            .catch(() => {});
          closeOverlay();
        },
      },
      {
        id: "reload-data",
        label: t("palette.cmd.reloadData"),
        group: t("palette.group.settings"),
        keywords: "refresh invalidate cache",
        run: () => {
          void queryClient.invalidateQueries();
          closeOverlay();
        },
      },
    ],
    [
      runAction,
      closeOverlay,
      router,
      queryClient,
      leftOpen,
      rightOpen,
      settings.expandThinking,
      settings.expandToolOutput,
      settings.density,
      update,
      // Every label is translated, so a language change has to rebuild the list.
      // Without this the palette keeps the labels it was first opened with, and the
      // *filter* keeps matching them — so a search would hit a row whose visible
      // text no longer contains what was typed.
      t,
    ],
  );

  const shortcutFor = useMemo(() => {
    const map = new Map<ShortcutAction, string>();
    for (const [combo, action] of Object.entries(settings.shortcuts)) {
      // First binding wins, so a user who adds a second combo for one action
      // still sees a stable hint rather than an arbitrary one.
      if (!map.has(action)) map.set(action, combo);
    }
    return map;
  }, [settings.shortcuts]);

  const ranked = useMemo(() => {
    if (!query.trim()) {
      // Recents float to the top of an unfiltered palette; that is the whole
      // reason to track them, and it makes repeated actions one keystroke away.
      const byId = new Map(commands.map((command) => [command.id, command]));
      const recents = recent.map((id) => byId.get(id)).filter((c): c is Command => Boolean(c));
      const rest = commands.filter((command) => !recent.includes(command.id));
      return [...recents, ...rest];
    }
    return fuzzyRank(commands, query, (command) => [
      command.label,
      command.group,
      command.keywords ?? "",
    ]).map((result) => result.item);
  }, [commands, query, recent]);

  // Clamp rather than reset-on-change: the cursor already returns to 0 with every
  // query edit, and this covers the remaining case where the list shrinks under
  // it (a command's label changed and it no longer matches).
  const activeIndex = Math.min(active, Math.max(0, ranked.length - 1));

  // A DOM side effect, which is what effects are for. It reads no state it also
  // writes, so nothing cascades.
  useEffect(() => scrollActiveIntoView(listRef.current, activeIndex), [activeIndex]);

  const setActive = useCallback(
    (index: number) => setPicker((current) => ({ ...current, active: index })),
    [],
  );

  function commit(index: number) {
    const command = ranked[index];
    if (!command) return;
    const next = [command.id, ...recent.filter((id) => id !== command.id)].slice(0, RECENT_LIMIT);
    setRecent(next);
    try {
      window.localStorage.setItem(RECENT_KEY, JSON.stringify(next));
    } catch {
      /* recency is best-effort */
    }
    command.run();
  }

  const { onKeyDown } = useListNavigation(ranked.length, commit);
  const grouped = !query.trim();

  return (
    <Overlay title={t("palette.title")} onClose={closeOverlay} describedBy="palette-hint">
      <div className="flex items-center gap-2 border-b border-line px-3 py-2.5">
        <span className="text-ink-muted">
          <CommandIcon size={15} />
        </span>
        <input
          type="text"
          role="combobox"
          aria-expanded
          aria-controls="palette-results"
          aria-activedescendant={ranked.length > 0 ? `palette-row-${activeIndex}` : undefined}
          aria-label={t("palette.field")}
          autoFocus
          value={query}
          onChange={(event) => setPicker({ query: event.target.value, active: 0 })}
          onKeyDown={(event) => onKeyDown(event, activeIndex, setActive)}
          placeholder={t("palette.placeholder")}
          className="flex-1 bg-transparent text-sm outline-none placeholder:text-ink-muted"
        />
      </div>

      <div
        ref={listRef}
        id="palette-results"
        role="listbox"
        aria-label={t("palette.list")}
        className="min-h-0 flex-1 pane py-1"
      >
        {ranked.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-ink-muted">{t("palette.none")}</p>
        ) : (
          ranked.map((command, index) => {
            const previous = index > 0 ? ranked[index - 1] : undefined;
            const isRecent = grouped && recent.includes(command.id);
            const wasRecent = previous ? grouped && recent.includes(previous.id) : false;
            const showHeading =
              grouped && (index === 0 || (isRecent !== wasRecent) || (!isRecent && command.group !== previous?.group));
            const heading = isRecent ? t("palette.group.recent") : command.group;
            const combo = command.action ? shortcutFor.get(command.action) : undefined;

            return (
              <div key={command.id}>
                {showHeading && (
                  <p className="px-3 pb-0.5 pt-2 text-3xs font-semibold uppercase tracking-wider text-ink-muted">
                    {heading}
                  </p>
                )}
                <div
                  id={`palette-row-${index}`}
                  data-index={index}
                  role="option"
                  aria-selected={index === activeIndex}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => commit(index)}
                  className={`flex cursor-pointer items-center gap-2 px-3 py-1.5 text-xs ${
                    index === activeIndex ? "bg-hover" : ""
                  }`}
                >
                  <span className="min-w-0 flex-1 truncate text-ink">{command.label}</span>
                  {combo && <Kbd>{formatCombo(combo, isMac)}</Kbd>}
                </div>
              </div>
            );
          })
        )}
      </div>

      <footer
        id="palette-hint"
        className="shrink-0 border-t border-line bg-sunken px-3 py-1.5 text-3xs text-ink-muted"
      >
        {t("palette.keys")}
      </footer>
    </Overlay>
  );
}
