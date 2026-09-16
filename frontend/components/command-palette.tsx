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
 * search several places for the best match. Both halves are `lib/select-list.ts`'s
 * doing rather than this file's: a heading is a `disabled` row, which is the same
 * flag that makes it take no cursor position and makes a query elide it. The version
 * this replaced re-derived all three from a `grouped` boolean at render time, in an
 * index space that was not the cursor's.
 */

import { useRouter } from "next/navigation";
import { useCallback, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useSettings, type ShortcutAction } from "@/lib/settings";
import { formatCombo, useIsMac } from "@/lib/shortcuts";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useT } from "@/lib/i18n";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";
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

/**
 * A group heading: a row that is rendered, introduces the rows under it, and cannot
 * be chosen.
 *
 * `disabled` is what makes it inert, and `buildRows` reads the same flag to decide
 * that it takes no cursor position and that a query elides it. That is exactly the
 * behaviour the palette hand-rolled — headings drawn only while unfiltered, and the
 * cursor numbered over commands rather than over rendered rows — except that here the
 * two facts are one fact. The version this replaced computed them in two places, in
 * two index spaces, which is the dual-index bug the shared engine exists to make
 * unrepresentable.
 */
const heading = (label: string): SelectChoice => ({
  label,
  disabled: true,
  separated: true,
  onSelect: () => {},
});

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

  const isMac = useIsMac();

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
        id: "adopt-session",
        label: t("palette.cmd.adoptSession"),
        group: t("palette.group.session"),
        action: "adoptSession",
        // `import` because that is what a user coming from another tool calls this, and
        // the endpoint it replaced was literally `POST /sessions/import`; `existing`
        // and `terminal` because the common case is a session started in a terminal.
        keywords: "import existing terminal native attach",
        run: () => runAction("adoptSession"),
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
        id: "find-in-agent",
        label: t("trace.findCurrentAgent"),
        group: t("palette.group.session"),
        action: "findInSession",
        run: () => runAction("findInSession"),
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
        keywords: "past previous history directory workdir",
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
        id: "refresh-environments",
        label: t("palette.cmd.refreshEnvironments"),
        group: t("palette.group.settings"),
        keywords: "machines agents connected reload",
        run: () => {
          // Re-reads which machines are connected and what each can run. A plain refetch
          // rather than a command that changes server state: an agent is available when
          // its CLI is on some machine's `PATH`, reported by that machine's envagent, so
          // there is no discovery pass to trigger. Worth keeping because a laptop that
          // just woke reports differently.
          void queryClient.invalidateQueries({ queryKey: ["environments"] });
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

  const run = useCallback(
    (command: Command) => {
      const next = [command.id, ...recent.filter((id) => id !== command.id)].slice(0, RECENT_LIMIT);
      setRecent(next);
      try {
        window.localStorage.setItem(RECENT_KEY, JSON.stringify(next));
      } catch {
        /* recency is best-effort */
      }
      command.run();
    },
    [recent],
  );

  const items = useMemo<SelectChoice[]>(() => {
    const rows: SelectChoice[] = [];
    const row = (command: Command): SelectChoice => ({
      label: command.label,
      // `meta`, not `hint`: the shortcut renders as a `<Kbd>`, and `hint` is a string
      // the filter matches — `mod` and `shift` would then be query terms that hit
      // every bound command at once.
      meta: command.action
        ? shortcutFor.has(command.action)
          ? <Kbd>{formatCombo(shortcutFor.get(command.action)!, isMac)}</Kbd>
          : undefined
        : undefined,
      // The group joins the searchable text here because a query elides the heading
      // that would otherwise carry it: `view` has to keep reaching the view commands
      // once their heading is gone. `keywords` rather than `hint` for the same reason
      // as above — these are aliases, not something the row shows.
      keywords: [command.group, command.keywords ?? ""].join(" "),
      onSelect: () => run(command),
    });

    // Recents float to the top of an unfiltered palette; that is the whole reason to
    // track them, and it makes repeated actions one keystroke away. They are excluded
    // from their own groups below, so no command is offered twice.
    const byId = new Map(commands.map((command) => [command.id, command]));
    const recents = recent.map((id) => byId.get(id)).filter((c): c is Command => Boolean(c));
    if (recents.length > 0) {
      rows.push(heading(t("palette.group.recent")));
      rows.push(...recents.map(row));
    }

    let group: string | undefined;
    for (const command of commands) {
      if (recent.includes(command.id)) continue;
      if (command.group !== group) {
        group = command.group;
        rows.push(heading(group));
      }
      rows.push(row(command));
    }
    return rows;
  }, [commands, recent, shortcutFor, isMac, run, t]);

  // `searchable: true`, never `auto`: the threshold counts single-select rows and a
  // palette marks none of them current, so the heuristic would answer "no filter" for
  // the one surface whose entire purpose is a query.
  //
  // No `onClose` — the list does not own the surface. Every command either closes the
  // overlay itself or dispatches an action that does, and two of them deliberately do
  // not (density, thinking) because their effect is visible behind the palette.
  // Escape belongs to `Overlay`.
  const list = useSelectList(items, { searchable: true });

  return (
    <Overlay title={t("palette.title")} onClose={closeOverlay} describedBy="palette-hint">
      <SelectFilter
        list={list}
        name={t("palette.list")}
        // Not "Filter commands": the field runs one, which is a different promise to
        // make to a screen reader than narrowing a set.
        label={t("palette.field")}
        placeholder={t("palette.placeholder")}
        icon={<CommandIcon size={15} />}
        size="roomy"
      />

      <SelectScroll list={list} label={t("palette.list")} fill>
        {/* The palette's own wording, not the shared "No matches": this surface is
            reached by muscle memory, and naming what was searched is the difference
            between "you typed something unknown" and "the palette is broken". */}
        {list.rows.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-ink-muted">{t("palette.none")}</p>
        ) : (
          <SelectRows list={list} />
        )}
      </SelectScroll>

      <footer
        id="palette-hint"
        className="shrink-0 border-t border-line bg-sunken px-3 py-1.5 text-3xs text-ink-muted"
      >
        {t("palette.keys")}
      </footer>
    </Overlay>
  );
}
