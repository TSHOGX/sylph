"use client";

/**
 * The agent's own slash commands, offered as the user types.
 *
 * The list comes from a live handshake with the session's agent, so it holds the
 * project's real skills, project commands, and plugins — dozens on a working
 * checkout, against the one or two a static descriptor used to declare. Nothing
 * here is hardcoded, which is what makes it correct for an agent this UI has never
 * heard of.
 *
 * # Why an inline menu rather than a dialog
 *
 * A slash command is a way of writing a message, not a separate operation. Typing
 * `/ver` and pressing Tab should complete, the way it does in the terminal this is
 * modelled on — and a modal that steals focus from the textarea breaks exactly
 * that. The menu is a listbox the textarea *owns*, announced through
 * `aria-activedescendant`, so focus never leaves the input.
 */

import { useEffect, useMemo, useRef } from "react";
import type { SessionCommand } from "@/lib/api";
import { fuzzyRank } from "@/lib/fuzzy";
import { useT } from "@/lib/i18n";

interface Props {
  /** Every command the agent reported. */
  commands: SessionCommand[];
  /** What the user typed after the leading slash. */
  query: string;
  /** Index of the highlighted row, owned by the composer so keys work in the input. */
  activeIndex: number;
  onActiveIndexChange: (index: number) => void;
  onPick: (command: SessionCommand) => void;
  /** Id the textarea points `aria-activedescendant` at. */
  listboxId: string;
}

/** How many rows to show.
 *
 * A real project reports dozens, and a menu listing all of them covers the
 * transcript it is meant to sit beside. Ranking means the intended command is
 * almost always in the first few. */
const VISIBLE = 8;

/**
 * Rank commands against what has been typed.
 *
 * A plain function, not a hook, so the ranking rule can be tested without a DOM —
 * the hook below is only the memo around it.
 */
export function slashMatches(
  commands: SessionCommand[],
  query: string,
): SessionCommand[] {
  if (commands.length === 0) return [];
  // `fuzzyRank` returns scored wrappers; only the items matter here. Field order is
  // the ranking: the name first, so typing `ver` prefers a command *called* verify
  // over one that merely mentions verifying, then the agent's own aliases, then the
  // description.
  //
  // Aliases are matched because the terminal matches them — `/usage` answers to
  // `cost` and `stats` — and a menu that finds nothing for a name the agent accepts
  // reads as a missing command.
  const ranked = query
    ? fuzzyRank(commands, query, (command) => [
        command.name,
        ...(command.aliases ?? []),
        command.description ?? "",
      ]).map((result) => result.item)
    : commands;
  return ranked.slice(0, VISIBLE);
}

export function useSlashMatches(
  commands: SessionCommand[],
  query: string,
): SessionCommand[] {
  return useMemo(() => slashMatches(commands, query), [commands, query]);
}

export function SlashMenu({
  commands,
  query,
  activeIndex,
  onActiveIndexChange,
  onPick,
  listboxId,
}: Props) {
  const matches = useSlashMatches(commands, query);
  const activeRef = useRef<HTMLLIElement>(null);
  const t = useT();

  // Keep the highlighted row visible as the arrow keys move past the fold.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (matches.length === 0) return null;

  return (
    <ul
      id={listboxId}
      role="listbox"
      aria-label={t("composer.slash.regionLabel")}
      className="max-h-64 overflow-y-auto overscroll-contain rounded-lg border border-line bg-raised py-1 shadow-lg"
    >
      {matches.map((command, index) => {
        const active = index === activeIndex;
        return (
          <li
            key={command.name}
            id={`${listboxId}-${index}`}
            ref={active ? activeRef : undefined}
            role="option"
            aria-selected={active}
            // Mouse down rather than click: a click fires after the textarea has
            // already lost focus, and losing focus is what closes this menu.
            onMouseDown={(event) => {
              event.preventDefault();
              onPick(command);
            }}
            onMouseEnter={() => onActiveIndexChange(index)}
            className={`flex cursor-pointer items-baseline gap-2 px-3 py-1.5 text-xs ${
              active ? "bg-active text-ink" : "text-ink-muted"
            }`}
          >
            <span className="font-mono font-medium text-ink">/{command.name}</span>
            {command.argHint ? (
              <span className="font-mono text-ink-faint">{command.argHint}</span>
            ) : null}
            {/* A control command opens a picker rather than sending a message, and
                that is worth saying before it is chosen: the two look identical in a
                list and behave nothing alike. */}
            {command.kind === "control" ? (
              <span className="shrink-0 rounded border border-line px-1 text-3xs text-ink-faint">
                {t("composer.setting")}
              </span>
            ) : null}
            {command.description ? (
              <span className="min-w-0 flex-1 truncate">{command.description}</span>
            ) : null}
            {command.source ? (
              <span className="shrink-0 text-ink-faint">{command.source}</span>
            ) : null}
          </li>
        );
      })}
    </ul>
  );
}
