"use client";

/**
 * The rendering half of a choice list: a filter field, the rows, and the one cursor
 * that ties them together.
 *
 * The rule this implements lives in `lib/select-list.ts`, which decides what is
 * shown and how it is numbered without a DOM. What is left here is the part that
 * genuinely needs one — where focus goes, when the active row is scrolled into view,
 * and which ARIA pattern the surface presents.
 *
 * # Two ARIA patterns, chosen by whether there is a text field
 *
 * Without a filter this is a menu: `menu`/`menuitem`, `menuitemradio` for a single
 * choice, container holds focus, rows are not tab stops.
 *
 * With a filter it must not be. A `role="menu"` may only contain menu items, so an
 * `<input>` inside one is invalid ARIA — axe reports it, and a screen reader is left
 * with a control that its container says cannot exist. The surface therefore becomes
 * the combobox pattern instead: the input is the `combobox`, the rows are a
 * `listbox` of `option`s, the cursor rides `aria-activedescendant`, and the current
 * value is `aria-selected` rather than `aria-checked`.
 *
 * Same rows, same cursor, same keys; only the roles differ. Which is why they share
 * a renderer rather than being two components that drift.
 */

import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useT } from "@/lib/i18n";
import { useIsMobile } from "@/lib/viewport";
import { scrollActiveIntoView } from "@/components/overlay";
import {
  buildRows,
  initialIndex,
  shouldSearch,
  type SelectChoice,
  type SelectRow,
} from "@/lib/select-list";
import { CheckIcon, SearchIcon } from "@/components/icons";

export interface SelectList<T extends SelectChoice> {
  /** Whether this list is filterable, and so which ARIA pattern it presents. */
  search: boolean;
  query: string;
  setQuery: (query: string) => void;
  /** Rows in render order, each carrying its own cursor position. */
  rows: SelectRow<T>[];
  /** The cursor, in row-index space. `-1` when nothing is selectable. */
  activeIndex: number;
  setActiveIndex: (index: number) => void;
  listId: string;
  rowId: (index: number) => string;
  /** For `aria-activedescendant`. Undefined when there is no active row. */
  activeDescendant: string | undefined;
  /** Choose a row by cursor position. */
  commit: (index: number) => void;
  /** Arrow keys, Home/End, Enter, Escape. Attach to whichever node holds focus. */
  onKeyDown: (event: KeyboardEvent) => void;
}

/**
 * Where the query lives when the caller does the ranking.
 *
 * Handed in as one object rather than as a `filter: "caller"` flag plus a query,
 * because those two are not independent: a query with hook-owned filtering would be
 * filtered twice, and caller-owned filtering with no query has nothing to read. One
 * parameter makes the invalid pair unrepresentable.
 */
export interface ExternalQuery {
  query: string;
  setQuery: (query: string) => void;
}

/**
 * Wire up one choice list.
 *
 * `onClose` is optional because not every list owns its surface: a menu closes on
 * selection, while the config dialog is closed by the mutation the selection
 * triggers. Passing it here rather than at each row keeps "act, then tear down" in
 * one place, in the one order that works — the action first, so a handler that reads
 * the surface still can.
 */
export function useSelectList<T extends SelectChoice>(
  items: readonly T[],
  {
    searchable = "auto",
    onClose,
    external,
  }: {
    searchable?: boolean | "auto";
    onClose?: () => void;
    /**
     * Take the rows as already ranked, and let the caller own the query.
     *
     * # Why this exists, when the whole point was one filtering rule
     *
     * The search overlay cannot be filtered here, and not for a stylistic reason.
     * Its rows come from two sources ranked by different engines — live sessions by
     * `fuzzyRank` in the browser, archive hits by TraceDB's full-text search over
     * event bodies that were never sent to the client — and it fixes live above
     * archive because a session you can open beats one you must import first. Ranking
     * that merged list by `buildRows` would re-score archive hits against the only
     * text the browser has, their titles, which is precisely the text that did *not*
     * match. A hit found by its transcript would then rank last or vanish.
     *
     * The query also has to leave this hook either way: it drives a debounced
     * server request, so the caller needs to read it. Owning it there and passing it
     * back in is what keeps one cursor over one row list — which is the invariant
     * this module actually protects. `buildRows` still numbers the rows, headings
     * still elide, the cursor still cannot be counted in a second space.
     *
     * Absent for every other list, and it should stay rare: a caller that ranks its
     * own rows also owns keeping `detail` searchable, which is the guarantee the row
     * model exists to make.
     */
    external?: ExternalQuery;
  } = {},
): SelectList<T> {
  const baseId = useId();
  const listId = `${baseId}-list`;

  const search = shouldSearch(items, searchable);

  /**
   * Query and cursor in one state object.
   *
   * Not two `useState`s: "a query edit resets the cursor" is then a single update
   * rather than an effect that repaints one frame with the stale cursor — the frame
   * a fast typist presses Enter on. The overlays here already settled on this shape
   * for the same reason.
   *
   * The cursor opens on the current choice, not at the top, so a twenty-model
   * catalogue opens showing where the user already is.
   */
  const [state, setState] = useState(() => ({ query: "", active: initialIndex(items) }));

  const query = external ? external.query : state.query;
  // An externally ranked list is filtered already, so it is built with no query: a
  // second pass would drop rows the caller's own ranking kept. Everything else
  // `buildRows` does — numbering, heading elision, one checked row per group — still
  // applies, which is why this is the same call rather than a bypass.
  const rows = useMemo(
    () => buildRows(items, external ? "" : state.query),
    [items, external, state.query],
  );
  const count = useMemo(() => rows.reduce((n, row) => (row.index === null ? n : n + 1), 0), [rows]);

  const activeIndex = count === 0 ? -1 : Math.min(state.active, count - 1);

  const externalSetQuery = external?.setQuery;
  const setQuery = useCallback(
    (next: string) => {
      // Clearing the filter returns the cursor to the current choice, the same place
      // the list opened on; a query moves it to the top match. Both halves happen in
      // one update, so no frame renders the new rows under the old cursor — the frame
      // a fast typist presses Enter on.
      setState({ query: next, active: next.trim() ? 0 : initialIndex(items) });
      externalSetQuery?.(next);
    },
    [items, externalSetQuery],
  );

  const setActiveIndex = useCallback((active: number) => {
    setState((current) => ({ ...current, active }));
  }, []);

  const commit = useCallback(
    (index: number) => {
      const row = rows.find((candidate) => candidate.index === index);
      if (!row || row.item.disabled) return;
      // The action runs before the surface tears down, so a handler that reads the
      // DOM or the surface's own state still can.
      row.item.onSelect();
      onClose?.();
    },
    [rows, onClose],
  );

  const onKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // Only claimed when this list actually owns a surface to close. A list that
        // does not — the config dialog's, whose `Overlay` owns dismissal — must let
        // the key bubble: stopping propagation and then having no `onClose` to call
        // swallows Escape outright and makes the dialog impossible to dismiss by
        // keyboard (WCAG 2.1.2).
        if (!onClose) return;
        // Stopped so it closes this surface only, not an overlay behind it.
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key === "Enter") {
        event.preventDefault();
        commit(activeIndex);
        return;
      }
      // Space activates a menu row, because a menu has no text field and Space is
      // one of the two keys ARIA prescribes for `menuitem`. In a filterable list it
      // must type instead — model descriptions are full of spaces, and a Space that
      // committed made the field unusable for exactly the lists that need it.
      if (event.key === " " && !search) {
        event.preventDefault();
        commit(activeIndex);
        return;
      }
      if (count === 0) return;
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActiveIndex((Math.max(activeIndex, 0) + 1) % count);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        setActiveIndex((Math.max(activeIndex, 0) - 1 + count) % count);
      } else if (event.key === "Home") {
        event.preventDefault();
        setActiveIndex(0);
      } else if (event.key === "End") {
        event.preventDefault();
        setActiveIndex(count - 1);
      }
    },
    [activeIndex, commit, count, onClose, search, setActiveIndex],
  );

  // Keep the cursor visible as it moves past the fold, and on open, where the
  // current choice may be well down a long catalogue.
  //
  // The container is found by the id it already carries for `aria-controls` rather
  // than through a ref the caller has to thread onto it. That is not only less
  // plumbing: a hook that returned a ref would have every consumer reading
  // `list.listRef` during render, which is what `react-hooks/refs` forbids, and the
  // id has to be correct anyway or the ARIA relationship is broken.
  useEffect(() => {
    if (activeIndex < 0) return;
    scrollActiveIntoView(document.getElementById(listId), activeIndex);
  }, [activeIndex, query, listId]);

  return {
    search,
    query,
    setQuery,
    rows,
    activeIndex,
    setActiveIndex,
    listId,
    rowId: (index: number) => `${baseId}-${index}`,
    activeDescendant: activeIndex >= 0 ? `${baseId}-${activeIndex}` : undefined,
    commit,
    onKeyDown,
  };
}

/**
 * The filter field.
 *
 * # Why focus is an effect and not `autoFocus`
 *
 * React's `autoFocus` fires at mount, and an anchored popover is `invisible` at
 * mount because it has not been measured yet — `visibility: hidden` elements cannot
 * take focus, so the call silently did nothing and every keystroke went to the
 * trigger instead. That is the same trap already recorded for the menu container in
 * `ui/menu.tsx`; a text field walks into it identically. `ready` is the caller's
 * "the surface is visible now", and focusing waits for it.
 *
 * # Why not on a phone
 *
 * Focusing raises the on-screen keyboard, and iOS Safari does not resize the layout
 * viewport when it appears — so a sheet sized in `dvh` keeps its height and its list
 * ends up behind the keyboard. Avoiding that is the whole reason the mobile form is
 * a sheet. The field sits at the top of it and one tap focuses it, which is what
 * every native picker with a search field does.
 *
 * # Why it is deferred past the commit
 *
 * React runs child effects before parent effects, and a modal parent records the
 * element focus came *from* in its own mount effect so it can hand focus back on
 * close. Focusing synchronously here therefore runs first, and the dialog captures
 * this field as its opener — then "restores" focus to a node that is unmounting, so
 * closing the picker dropped focus on `<body>` (WCAG 2.4.3). A microtask is enough
 * to land after the whole commit, including every ancestor's effects, without ever
 * showing an unfocused frame.
 */
export function SelectFilter<T extends SelectChoice>({
  list,
  name,
  label,
  placeholder,
  icon,
  size = "compact",
  trailing,
  ready = true,
}: {
  // Generic, like `SelectRows` and `SelectScroll`. Pinned to the base type it
  // silently relied on array covariance to accept a richer list, which is the kind
  // of asymmetry that reads as "this one cannot be used here".
  list: SelectList<T>;
  /** What is being filtered, for the field's own accessible name. */
  name: string;
  /**
   * Replaces the composed "Filter {name}" name outright.
   *
   * For the field that is not a filter. The search overlay's query reaches TraceDB's
   * full-text index over transcripts the browser never loaded, and the palette's runs
   * a command rather than narrowing a set — calling either "Filter …" would name the
   * wrong operation for a screen reader, which is the only place this string is read.
   */
  label?: string;
  /**
   * Overrides the generic "Type to filter".
   *
   * Worth a prop because the useful hint is list-specific: a workspace list is
   * filtered by *any part of the path*, which is not what a user assumes from a
   * list whose visible column is a leaf name.
   */
  placeholder?: string;
  /** Leading glyph. Decorative; defaults to the magnifier. */
  icon?: ReactNode;
  /**
   * `roomy` for a full-surface overlay, whose field is the first thing on screen
   * rather than a strip above a popover's rows.
   */
  size?: "compact" | "roomy";
  /**
   * Status beside the field: "searching archive…", a spinner.
   *
   * Here rather than in the caller's own bar because it belongs to the query, and a
   * separate row above the list would move the rows down the moment a request
   * started.
   */
  trailing?: ReactNode;
  ready?: boolean;
}) {
  const t = useT();
  const isMobile = useIsMobile();
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!ready || isMobile) return;
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) inputRef.current?.focus();
    });
    return () => {
      cancelled = true;
    };
  }, [ready, isMobile]);

  return (
    <div
      className={`flex shrink-0 items-center gap-2 border-b border-line px-3 ${
        size === "roomy" ? "py-2.5" : "py-2"
      }`}
    >
      <span aria-hidden className="shrink-0 text-ink-muted">
        {icon ?? <SearchIcon size={size === "roomy" ? 15 : 14} />}
      </span>
      <input
        ref={inputRef}
        // `text` rather than `search`: the type-specific clear button is a control
        // with no accessible name, drawn by the browser inside a combobox.
        type="text"
        role="combobox"
        // Not a constant: `aria-expanded` claims a popup is showing, and with no
        // choosable row there is no `listbox` for `aria-controls` to point at — the
        // rows are replaced by a "no matches" line, which is not an option list.
        // Hard-coding it true made a screen reader announce a list that was not
        // rendered.
        aria-expanded={list.rows.length > 0}
        aria-controls={list.listId}
        aria-activedescendant={list.activeDescendant}
        // Named for what it filters, and deliberately not the same string as the
        // list itself — two controls a screen reader cannot tell apart is the
        // collision that once failed 40 specs at once.
        aria-label={label ?? t("select.filter.label", { name })}
        autoComplete="off"
        spellCheck={false}
        value={list.query}
        onChange={(event) => list.setQuery(event.target.value)}
        onKeyDown={list.onKeyDown}
        placeholder={placeholder ?? t("select.filter.placeholder")}
        // `text-base` under a coarse pointer: mobile Safari zooms the page when a
        // focused field's text is under 16px and does not zoom back out.
        className={`min-w-0 flex-1 bg-transparent text-base outline-none placeholder:text-ink-muted ${
          size === "roomy" ? "md:text-sm" : "md:text-xs"
        }`}
      />
      {trailing}
    </div>
  );
}

/**
 * The rows.
 *
 * One renderer for both patterns, because the difference between them is three
 * attributes and everything a row *does* — hover moves the cursor, click commits,
 * the check column reserves space only when something is checkable — is identical.
 *
 * # Why a row shape rather than a render callback
 *
 * Every richer picker in the app — workspaces, archived sessions, the palette —
 * originally forked the whole engine because this renderer could only draw
 * `icon / label / hint`, and a workspace needs its path on a second line. The seam
 * that fixed that is `detail` and `meta` on the row *model*, not a `renderRow`
 * escape hatch, and the difference matters: `detail` is part of the searchable
 * contract in `lib/select-list.ts`, so a two-line row cannot render text the
 * filter is unable to reach. A callback would have let exactly that back in, and
 * would also have handed each call site the ARIA attributes and the cursor wiring
 * to get subtly wrong — which is how the four hand-rolled comboboxes drifted.
 */
export function SelectRows<T extends SelectChoice>({
  list,
  density = "compact",
}: {
  list: SelectList<T>;
  /** `comfortable` for a dialog, which is a wider surface than a popover. */
  density?: "compact" | "comfortable";
}) {
  const t = useT();
  // Reserve the check column only when something here is checkable, so an action
  // menu is not indented for a state it never shows.
  const checkable = list.rows.some((row) => row.item.checked !== undefined);
  // Derived per list, not per row, so a filtered result whose matches happen to
  // carry no detail line does not shrink its rows mid-list as the user types.
  const twoLine = list.rows.some(
    (row) => row.item.detail !== undefined || row.item.excerpt !== undefined,
  );
  const padding = density === "comfortable" ? "px-4 py-2" : "px-3 py-1.5";

  if (list.rows.length === 0) {
    return (
      <p className={`${padding} text-center text-xs text-ink-muted`}>{t("select.filter.none")}</p>
    );
  }

  return (
    <>
      {list.rows.map(({ item, index }, position) => {
        const active = index !== null && index === list.activeIndex;
        const role = list.search
          ? "option"
          : item.checked === undefined
            ? "menuitem"
            : "menuitemradio";
        return (
          <div key={`${item.label}-${position}`}>
            {/* A `separator` is a legal child of a `menu` and *not* of a `listbox`,
                which admits only options and groups — axe reports
                `aria-required-children` on the whole list, one of the two patterns at
                a time. So in the combobox pattern the same rule is drawn and not
                announced: the line is decoration either way, and a screen reader in a
                listbox already hears each option's position out of the total. */}
            {item.separated &&
              (list.search ? (
                <div aria-hidden className="my-1 border-t border-line" />
              ) : (
                <div role="separator" className="my-1 border-t border-line" />
              ))}
            <button
              type="button"
              id={index === null ? undefined : list.rowId(index)}
              // What `scrollActiveIntoView` looks the active row up by.
              data-index={index ?? undefined}
              role={role}
              // A listbox marks the chosen value with `aria-selected`; a menu with
              // `aria-checked`. Using the menu attribute on an option would be
              // invalid, and neither is the cursor — that is `aria-activedescendant`.
              aria-selected={list.search ? item.checked : undefined}
              aria-checked={list.search ? undefined : item.checked}
              disabled={item.disabled}
              // The input or the container owns focus; rows carry a virtual cursor.
              tabIndex={-1}
              onMouseEnter={() => index !== null && list.setActiveIndex(index)}
              onClick={() => index !== null && list.commit(index)}
              className={`tap-row flex w-full items-center gap-2 text-left text-xs ${padding} ${
                item.disabled
                  ? "cursor-default font-medium text-ink-faint"
                  : item.danger
                    ? active
                      ? "bg-danger-bg text-danger"
                      : "text-danger"
                    : active
                      ? "bg-hover text-ink"
                      : "text-ink-soft"
              }`}
            >
              {checkable && (
                <span
                  aria-hidden
                  className={`w-4 shrink-0 text-ink-muted ${twoLine ? "mt-0.5 self-start" : ""}`}
                >
                  {item.checked && <CheckIcon size={14} />}
                </span>
              )}
              {item.icon && (
                <span
                  aria-hidden
                  // Top-aligned on a two-line row so the glyph sits beside the
                  // label rather than floating between the two lines.
                  className={`shrink-0 text-ink-muted ${twoLine ? "mt-0.5 self-start" : ""}`}
                >
                  {item.icon}
                </span>
              )}
              {/* One line collapses to the label itself, so a menu row keeps the
                  exact box it had before `detail` existed — no wrapper, no extra
                  baseline to disagree about. */}
              {item.detail === undefined &&
              item.meta === undefined &&
              item.excerpt === undefined ? (
                <span className="min-w-0 flex-1 truncate">{item.label}</span>
              ) : (
                <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span className="flex min-w-0 items-baseline gap-2">
                    <span className="min-w-0 flex-1 truncate">{item.label}</span>
                    {item.meta !== undefined && (
                      <span className="shrink-0 text-3xs text-ink-muted">{item.meta}</span>
                    )}
                  </span>
                  {item.detail !== undefined && (
                    // `title` so the full path is recoverable on hover: the line is
                    // truncated, and a path is the one string where the elided end
                    // is often the part that disambiguates.
                    <span
                      title={item.detail}
                      className="truncate font-mono text-3xs text-ink-muted"
                    >
                      {item.detail}
                    </span>
                  )}
                  {/* Not mono: an excerpt is prose from a transcript, and the path
                      above it is the line that needs the fixed width. */}
                  {item.excerpt !== undefined && (
                    <span className="truncate text-3xs text-ink-muted">{item.excerpt}</span>
                  )}
                </span>
              )}
              {item.hint && (
                <span
                  className={`max-w-[45%] shrink-0 truncate font-mono text-3xs text-ink-muted ${
                    twoLine ? "mt-0.5 self-start" : ""
                  }`}
                >
                  {item.hint}
                </span>
              )}
            </button>
          </div>
        );
      })}
    </>
  );
}

/**
 * The scroll container.
 *
 * This is the fix for the reported bug: the panel is `overflow-hidden` for its
 * rounded corners, and with no scrolling child a catalogue longer than the panel was
 * simply cut off with no way to reach the rest. The cap is in `dvh` as well as `rem`
 * so a short viewport bounds it too, and `overscroll-contain` stops a flick that
 * reaches the end from scrolling whatever is behind.
 */
export function SelectScroll<T extends SelectChoice>({
  list,
  label,
  fill = false,
  children,
}: {
  list: SelectList<T>;
  /** Names the list for a screen reader. */
  label: string;
  /** Fill the available height instead of capping it — the mobile sheet and dialog. */
  fill?: boolean;
  children: React.ReactNode;
}) {
  // A `listbox` may only contain options and groups, and this container also holds
  // the states that are *not* rows: a spinner, a retry button, "no matches". Claiming
  // the role while one of those is showing is the same invalid-ARIA class as an
  // `<input>` inside a `role="menu"` — axe reports `aria-required-children`, and a
  // screen reader is handed a list box with no options in it. So the role is claimed
  // only when there is something for it to contain.
  const listbox = list.search && list.rows.length > 0;
  return (
    <div
      id={list.listId}
      // A menu's role belongs to the surface, which owns focus and the label; only
      // the combobox pattern needs a role on the scrolling list itself.
      role={listbox ? "listbox" : undefined}
      aria-label={listbox ? label : undefined}
      className={
        fill
          ? "pane min-h-0 flex-1 py-1"
          : "max-h-[min(24rem,50dvh)] overflow-y-auto overscroll-contain py-1"
      }
    >
      {children}
    </div>
  );
}
