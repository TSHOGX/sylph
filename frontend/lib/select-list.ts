/**
 * The row model behind every choice list: menus, the config picker, and the
 * searchable model catalogue.
 *
 * # Why this is a module of its own
 *
 * A choice list has two rendering modes — unfiltered, where non-selectable rows
 * (group headings, separators) are part of the list, and filtered, where they are
 * not — and one cursor that has to mean the same thing in both. The first attempt
 * at search kept a cursor per mode and reconciled them at the call site, which put
 * the highlight, `aria-activedescendant`, and Enter on three different rows: the
 * unfiltered renderer numbered *every* item while the cursor counted only the
 * enabled ones.
 *
 * `buildRows` removes that class of bug by construction. It returns the rows in
 * render order, each already carrying its own cursor position — or `null` for a row
 * that cannot be chosen. No renderer computes an index, so there is only ever one
 * index space, and `activeIndex` cannot be interpreted against the wrong list.
 *
 * It is a pure module because that is what makes the rule testable: filtering,
 * heading elision, and cursor numbering are decided here and asserted in
 * `select-list.test.ts` without rendering anything.
 */

import type { ReactNode } from "react";
import { fuzzyRank } from "./fuzzy";

/**
 * What every choice list's rows have in common, minus the action.
 *
 * Split from `SelectChoice` so the pure rules below — filtering, numbering, which
 * row opens selected — can be stated and tested over plain data, with no function
 * to construct. `MenuItem` is `SelectChoice` with a couple of menu-only extras.
 */
export interface SelectOption {
  label: string;
  /** Leading icon. Decorative; the label is the accessible name. */
  icon?: ReactNode;
  /**
   * Second line under the label, and matched by the filter.
   *
   * This is the field that let every richer picker join this engine rather than
   * fork it. A workspace is identified by its *path*, not its leaf name — the
   * user's mental index of a project is often a middle segment (`huawei`,
   * `sylph`) that no leaf carries — and an archived session by the directory it
   * ran in. Both need a line the eye can read and the filter can reach, which
   * `hint` could not be: it is trailing, mono, and width-capped for a shortcut.
   *
   * Being part of the searchable contract is why it lives here rather than in a
   * render callback. A row that renders text the filter cannot see is a row the
   * user can read and not find.
   */
  detail?: string;
  /** Trailing hint — a shortcut, or the agent's own description of a choice. */
  hint?: string;
  /**
   * Words the filter matches but the row never shows.
   *
   * The command palette is why this exists: "Stop the running turn" has to be
   * reachable by `cancel` and `interrupt`, and "Quick Start session" by `scratch`.
   * Rendering those would double the width of every row to serve the one query that
   * used them, and every palette worth copying keeps its aliases hidden.
   *
   * It is the *last* field ranked, below the trailing hint, because a row that
   * matched here shows the user nothing explaining why — so it must never outrank a
   * row whose visible text matched. Keep it to synonyms of what the row already
   * says: a word here that is not a paraphrase of the label produces a result the
   * user cannot account for, which is the mirror image of the unsearchable `detail`
   * this model was built to prevent.
   */
  keywords?: string;
  /**
   * A third line: why this row matched, when that is not visible in the row itself.
   *
   * Only the archive search has one. Its hits are ranked server-side by full-text
   * search over the transcript, so the matched words are in event bodies the browser
   * never loaded — and without the snippet an archive row is a title and a path with
   * nothing saying why a query about a bug reached a session called "notes".
   *
   * Deliberately not matched, and not for `meta`'s reason. This text *is* the product
   * of a match; re-ranking a caller-ranked row by the excerpt the caller derived from
   * its own ranking would just launder that ranking through a weaker one.
   */
  excerpt?: string;
  /**
   * Trailing metadata: counts, a badge, a timestamp.
   *
   * Deliberately *not* matched. It is derived from the candidate rather than
   * naming it — "3 sessions" is true of hundreds of rows, so admitting it to the
   * filter would rank by a word that identifies nothing.
   */
  meta?: ReactNode;
  /** Renders as a dimmed, unfocusable row: the action exists but does not apply. */
  disabled?: boolean;
  /**
   * Marks this row as the current choice in a single-select group, which also
   * switches its role. `undefined` for a plain action, so an action is never
   * announced as an unchecked option.
   */
  checked?: boolean;
  /** Draws a separator above this row. */
  separated?: boolean;
  /** Destructive: stop, discard, delete. */
  danger?: boolean;
}

/**
 * A row that can be chosen.
 *
 * The renderer needs the action; the rules above do not, which is why they are typed
 * on `SelectOption`. A disabled row still carries one so a heading needs no special
 * case at its construction site — `commit` refuses to run it.
 */
export interface SelectChoice extends SelectOption {
  onSelect: () => void;
}

/**
 * Choices past which a list gets a filter field.
 *
 * Eight is where a list stops being scannable at a glance and starts being
 * searched — and it is below every real model catalogue (`claude` reports around a
 * dozen, `pi` twenty-one) while staying above the two-to-four-value knobs that make
 * up the rest of an agent's config, which a filter field would only clutter.
 */
export const SELECT_SEARCH_THRESHOLD = 8;

export interface SelectRow<T> {
  item: T;
  /**
   * The row's cursor position, or `null` when it cannot be chosen.
   *
   * A heading is a row with no position: it is rendered, but arrow keys pass over
   * it and it has no `data-index` for the cursor to scroll to.
   */
  index: number | null;
}

/**
 * How many rows form a single-select group.
 *
 * This, not the total row count, is what decides whether a filter field appears.
 * A menu of eight *actions* is a menu and should stay one; eight *choices* is a
 * catalogue. Disabled rows are excluded because a group heading is not a choice.
 */
export function choiceCount(items: readonly SelectOption[]): number {
  let count = 0;
  for (const item of items) {
    if (!item.disabled && item.checked !== undefined) count += 1;
  }
  return count;
}

/** Whether a list of this shape should show a filter field. */
export function shouldSearch(
  items: readonly SelectOption[],
  searchable: boolean | "auto",
): boolean {
  if (typeof searchable === "boolean") return searchable;
  return choiceCount(items) >= SELECT_SEARCH_THRESHOLD;
}

/**
 * At most one checked row per single-select group.
 *
 * A group is a run of rows between headings, because a flattened config menu
 * legitimately holds several single-select groups — model, then effort, then
 * permissions — and each of those has its own current value. Collapsing across the
 * whole list would uncheck all but the first option's.
 *
 * # Why the client enforces this at all
 *
 * `checked` arrives already computed, and every producer writes it as
 * `choice.value === currentValue` — a *filter*, which marks every match. That is
 * fine until an agent reports two choices with the same value, and one did: `pi`
 * offered `deepseek-v4-pro` from two different providers, so two rows showed a
 * checkmark and neither was identifiably the real one. The adapter now issues
 * provider-qualified values, which fixes the instance.
 *
 * This is the second layer, and it is not redundant. "A single choice has one current
 * row" is a property of single-select itself, not of any one agent's spelling, and
 * this client renders whatever an agent it has never seen reports — including agents
 * written by other people. Fixing only the adapter would leave the next one to
 * rediscover the same symptom.
 *
 * The first match wins. Something has to, and the first row is the one a user
 * scanning from the top would take to be current.
 */
function withOneChecked<T extends SelectOption>(items: readonly T[]): readonly T[] {
  let seenInGroup = false;
  let changed = false;
  const out = items.map((item) => {
    // A heading ends the group, so the next run gets its own current row.
    if (item.disabled) {
      seenInGroup = false;
      return item;
    }
    if (item.checked !== true) return item;
    if (!seenInGroup) {
      seenInGroup = true;
      return item;
    }
    changed = true;
    return { ...item, checked: false };
  });
  // The same array back when nothing collided, so the common case allocates nothing
  // and referential equality holds for callers that memoize on it.
  return changed ? out : items;
}

/**
 * The rows to render, in order, each numbered for the cursor.
 *
 * An empty query keeps the list as authored, headings included, and numbers only
 * the rows that can be chosen. A query ranks the choosable rows and drops the rest:
 * a heading introduces choices that are no longer beneath it, so keeping one in a
 * filtered result labels the wrong thing.
 *
 * Label, detail, hint, and keywords are all matched, because each carries words a
 * user actually remembers: a model catalogue's hint is the agent's own description
 * (a provider name, a price), and a workspace's detail is the path whose middle
 * segment is how the project is known. Field order is the ranking, so a label
 * match wins, then the detail line, then the trailing hint, then the invisible
 * keywords — which is the right order because it runs from what names the row, to
 * what annotates it, to what only aliases it.
 */
export function buildRows<T extends SelectOption>(
  items: readonly T[],
  query: string,
): SelectRow<T>[] {
  const trimmed = query.trim();
  // Before filtering, so grouping is decided on the authored list: a query drops the
  // headings that delimit the groups.
  const source = withOneChecked(items);

  if (!trimmed) {
    const rows: SelectRow<T>[] = [];
    let index = 0;
    for (const item of source) {
      rows.push({ item, index: item.disabled ? null : index });
      if (!item.disabled) index += 1;
    }
    return rows;
  }

  return fuzzyRank(
    source.filter((item) => !item.disabled),
    trimmed,
    (item) => [item.label, item.detail ?? "", item.hint ?? "", item.keywords ?? ""],
  ).map((result, index) => ({ item: result.item, index }));
}

/** The cursor position a list should open on: the current choice, else the top. */
export function initialIndex(items: readonly SelectOption[]): number {
  const rows = buildRows(items, "");
  const current = rows.find((row) => row.index !== null && row.item.checked === true);
  return current?.index ?? 0;
}
