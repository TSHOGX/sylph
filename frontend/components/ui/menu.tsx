"use client";

/**
 * Menus: one behaviour, three ways of being positioned.
 *
 * - `ContextMenu` is anchored to a point (a right-click).
 * - `Menu` is anchored to a trigger element (a dropdown), and becomes a bottom
 *   sheet on a phone, where a 200px popover pinned to a 32px button is both hard
 *   to hit and prone to landing under the on-screen keyboard.
 * - Rows, the cursor, and filtering are `ui/select-list`, shared with the config
 *   dialog — which is not a menu, but is the same list.
 *
 * # Why this replaced every `<select>` in the composer
 *
 * The agent, model, and effort controls were native `<select>` elements, and a
 * `<select>` is sized by its *longest option*. Inside the composer's non-wrapping
 * flex row, one long model name therefore pushed Send past the right edge of the
 * viewport — the reported bug where the composer "runs off the screen on a phone".
 * That is not fixable with `min-w-0`, because the intrinsic width comes from
 * content the user cannot see. A button that renders the *current* value can
 * truncate; a `<select>` cannot.
 *
 * The other reason is that a native `<select>` cannot be styled: on iOS it renders
 * as a system wheel with its own typography, which is exactly the "assembled from
 * different kits" quality this pass is removing.
 *
 * # A long list scrolls, and a long catalogue is filterable
 *
 * The rows live in their own scroll container rather than in the panel. The panel is
 * `overflow-hidden` for its rounded corners, so without one a list taller than the
 * panel was clipped outright with no way to reach the rest — a model catalogue of a
 * dozen entries was simply unusable. Past `SELECT_SEARCH_THRESHOLD` choices a filter
 * field appears and the surface switches from the menu pattern to the combobox
 * pattern; see `ui/select-list.tsx` for why that switch is mandatory rather than
 * stylistic.
 */

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useIsMobile } from "@/lib/viewport";
import { Sheet } from "@/components/ui/sheet";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";

/**
 * A menu row.
 *
 * Structurally a `SelectChoice`; the alias is kept because it is what every call
 * site in the app already names, and because a menu row is the only one that can be
 * `danger`.
 */
export type MenuItem = SelectChoice;

/* ── Dismissal ────────────────────────────────────────────────────────────── */

/**
 * Dismiss on an outside press, on scroll, and on resize.
 *
 * `enabled` is not a convenience. This is for the *popover* only: a sheet already
 * has a scrim and a drag gesture, and its panel is not the node `ref` points at —
 * so with this active on the mobile path, `ref.current` was null, every
 * `contains()` was false, and the very pointerdown that began a tap on a menu item
 * closed the sheet before the click could land. The item unmounted mid-gesture and
 * its `onClick` never ran, so selecting a model silently did nothing on a phone.
 */
function useDismiss(
  ref: React.RefObject<HTMLElement | null>,
  onClose: () => void,
  enabled = true,
) {
  useEffect(() => {
    if (!enabled) return;

    /**
     * Outside dismiss on `click`, not `pointerdown`.
     *
     * A capture-phase `pointerdown` on the document runs before the row's own
     * `click`, so the menu unmounted mid-gesture and the selection never landed —
     * the same failure as the sheet above, and the reason the mobile path needs the
     * `enabled` guard at all. `click` fires after the row has been activated.
     */
    function onClick(event: MouseEvent) {
      if (!ref.current?.contains(event.target as Node)) onClose();
    }

    /**
     * Scroll detaches the menu from whatever it was anchored to — except when the
     * thing that scrolled is the menu's own list, which is the common case now that
     * a long catalogue scrolls inside the panel. Closing on that made the bottom of
     * every model list unreachable.
     */
    function onScroll(event: Event) {
      if (ref.current?.contains(event.target as Node)) return;
      onClose();
    }

    // Capture phase, because scroll does not bubble from the pane that scrolled.
    document.addEventListener("click", onClick, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("click", onClick, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onClose);
    };
  }, [ref, onClose, enabled]);
}

const PANEL =
  "z-50 flex min-w-48 max-w-[min(22rem,calc(100vw-1rem))] flex-col overflow-hidden rounded-xl border border-line bg-raised shadow-xl outline-none";

/* ── Point-anchored (right-click) ─────────────────────────────────────────── */

export function ContextMenu({
  x,
  y,
  items,
  onClose,
  label,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  label: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: x, top: y });

  // A context menu is a short list of actions on the thing that was right-clicked,
  // so it is never filterable: there is nothing to filter, and a text field would
  // take the focus the container needs for arrow keys.
  const list = useSelectList(items, { searchable: false, onClose });

  // Clamped after mount, when the real size is known. Guessing the height from the
  // click coordinates alone still clips a menu opened near the bottom edge.
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const rect = node.getBoundingClientRect();
    setPosition({
      left: Math.max(8, Math.min(x, window.innerWidth - rect.width - 8)),
      top: Math.max(8, Math.min(y, window.innerHeight - rect.height - 8)),
    });
    node.focus();
  }, [x, y]);

  useDismiss(ref, onClose);

  return (
    <div
      ref={ref}
      role="menu"
      aria-label={label}
      aria-activedescendant={list.activeDescendant}
      tabIndex={-1}
      style={{ left: position.left, top: position.top }}
      onKeyDown={list.onKeyDown}
      className={`fixed ${PANEL}`}
    >
      <SelectScroll list={list} label={label}>
        <SelectRows list={list} />
      </SelectScroll>
    </div>
  );
}

/* ── Trigger-anchored (dropdown), sheet on mobile ─────────────────────────── */

export function Menu({
  anchor,
  items,
  onClose,
  label,
  title,
  align = "start",
  searchable = "auto",
}: {
  /** The trigger. Positioning reads its rect; focus returns to it on close. */
  anchor: HTMLElement | null;
  items: MenuItem[];
  onClose: () => void;
  label: string;
  /** Sheet heading on mobile. Defaults to `label`. */
  title?: string;
  /** Which edge of the trigger the panel lines up with. */
  align?: "start" | "end";
  /** Show a filter field. `auto` turns it on for long single-select catalogues. */
  searchable?: boolean | "auto";
}) {
  const isMobile = useIsMobile();
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);

  const list = useSelectList(items, { searchable, onClose });

  /**
   * Measured after mount for the same reason as the context menu, plus one more:
   * the composer's controls sit at the bottom of the viewport, so a menu placed
   * below its trigger would open off-screen. It flips above when it does not fit.
   *
   * Deliberately *not* dependent on the query. Filtering changes the panel's height,
   * and re-measuring on every keystroke walked the panel up and down under the
   * cursor as the user typed. The list's own height is capped, so the panel opened
   * at its full size is the size it keeps — which makes one measurement correct for
   * the life of the menu.
   */
  useEffect(() => {
    if (isMobile) return;
    const node = ref.current;
    if (!node || !anchor) return;
    const trigger = anchor.getBoundingClientRect();
    const panel = node.getBoundingClientRect();
    const below = trigger.bottom + 6;
    const above = trigger.top - panel.height - 6;
    const fitsBelow = below + panel.height <= window.innerHeight - 8;
    const left = align === "end" ? trigger.right - panel.width : trigger.left;

    setPosition({
      left: Math.max(8, Math.min(left, window.innerWidth - panel.width - 8)),
      top: fitsBelow ? below : Math.max(8, above),
    });
  }, [anchor, align, isMobile]);

  /**
   * Focus the panel once it is actually visible.
   *
   * Deliberately a second effect rather than a `node.focus()` at the end of the one
   * above, and the reason is subtle enough to be worth recording: while `position`
   * is null the panel carries `invisible`, and `visibility: hidden` elements cannot
   * take focus. Focusing inside the measuring effect therefore ran against the
   * still-invisible element from the previous render and did nothing at all —
   * silently, since `focus()` does not report failure.
   *
   * The consequence was not cosmetic. `onKeyDown` is a React handler on this
   * container, so with focus left on the trigger, Escape never reached it and the
   * menu could not be dismissed by keyboard (WCAG 2.1.2).
   *
   * `focusedRef` so this fires once. Without it, a re-render that re-measures would
   * pull focus back out of a row the user had arrowed to.
   *
   * A filterable menu hands focus to its field instead — through the same
   * `position !== null` gate, passed down as `ready`, because React's `autoFocus`
   * fires at mount and would hit the invisible panel in exactly the way described
   * above. That is what made the first search implementation swallow every
   * keystroke.
   */
  const focusedRef = useRef(false);
  useEffect(() => {
    if (isMobile || position === null || focusedRef.current || list.search) return;
    focusedRef.current = true;
    ref.current?.focus();
  }, [position, isMobile, list.search]);

  /**
   * Hand focus back to the trigger when the menu closes.
   *
   * A menu that takes focus and does not return it drops a keyboard user at the top
   * of the document (WCAG 2.4.3). It also breaks the *next* surface in a chain: an
   * item that opens a dialog has the menu unmount in the same commit the dialog
   * mounts, and the dialog captures `document.activeElement` as the opener it will
   * later restore to. With focus on a disappearing menu, that capture is `<body>`,
   * so closing the dialog stranded focus there.
   *
   * React runs every cleanup in a commit before every effect, so this restores the
   * trigger before the dialog reads `activeElement` — which is what makes the chain
   * end where it began. `isConnected` guards a trigger unmounted by the action
   * itself; focusing a detached node silently moves focus to `<body>`.
   */
  useEffect(() => {
    return () => {
      if (anchor?.isConnected) anchor.focus();
    };
  }, [anchor]);

  // Popover only. The sheet owns its own dismissal, and this would fight the tap.
  useDismiss(ref, onClose, !isMobile);

  const filter = list.search ? (
    <SelectFilter list={list} name={label} ready={position !== null} />
  ) : null;

  // On a phone the same items become a bottom sheet: reachable one-handed, rows at
  // full touch height, and never clipped by the on-screen keyboard.
  if (isMobile) {
    return (
      <Sheet title={title ?? label} onClose={onClose}>
        {filter}
        {/*
          The role sits on the scrolling list in the combobox pattern and on a
          wrapper in the menu pattern, so a sheet gets one or the other and never
          both — a `role="menu"` around a `listbox` would nest two patterns.
        */}
        {list.search ? (
          <SelectScroll list={list} label={label} fill>
            <SelectRows list={list} />
          </SelectScroll>
        ) : (
          <div
            role="menu"
            aria-label={label}
            aria-activedescendant={list.activeDescendant}
            onKeyDown={list.onKeyDown}
            className="flex min-h-0 flex-1 flex-col"
          >
            <SelectScroll list={list} label={label} fill>
              <SelectRows list={list} />
            </SelectScroll>
          </div>
        )}
      </Sheet>
    );
  }

  const panel = (
    <div
      ref={ref}
      // In the combobox pattern the field is the control and the rows are its
      // listbox, so the panel is a plain container: a `role="menu"` here would
      // declare a menu that contains an `<input>`, which is invalid and which axe
      // reports.
      role={list.search ? undefined : "menu"}
      aria-label={list.search ? undefined : label}
      aria-activedescendant={list.search ? undefined : list.activeDescendant}
      tabIndex={list.search ? undefined : -1}
      // The field owns the keys when there is one; it is where focus is.
      onKeyDown={list.search ? undefined : list.onKeyDown}
      // Hidden until measured, so it does not flash at the wrong place for a frame.
      style={position ?? { left: -9999, top: 0 }}
      className={`fixed ${PANEL} ${position ? "" : "invisible"}`}
    >
      {filter}
      <SelectScroll list={list} label={label}>
        <SelectRows list={list} />
      </SelectScroll>
    </div>
  );

  // Portalled to the body: the composer's controls sit inside `overflow-hidden`
  // scrolling ancestors, which would clip a panel positioned against the viewport.
  return createPortal(panel, document.body);
}

/**
 * Trigger + menu, with the open state and the anchor ref bound together.
 *
 * Every dropdown needs the same three pieces of bookkeeping — a boolean, a ref to
 * the trigger, and `aria-haspopup`/`aria-expanded` on it. Hoisting them here is
 * what keeps the composer's controls from re-implementing it four times.
 */
export function MenuButton({
  label,
  title,
  items,
  align,
  searchable = "auto",
  children,
}: {
  /**
   * Names the menu for a screen reader, and titles the sheet on mobile.
   *
   * Note this is the *menu's* name, not the trigger's — the trigger carries its own
   * `aria-label`. They are usually the same, but must be allowed to differ: a
   * trigger needs the shortest name that identifies it, while a sheet heading needs
   * a phrase. Conflating them produced a real bug — a `+` trigger named "Add to this
   * message" collided with the composer textarea's own "Message" label, leaving two
   * controls whose accessible names could not be told apart.
   */
  label: string;
  /** Overrides the sheet's heading when the menu's name is too terse for one. */
  title?: string;
  items: MenuItem[];
  align?: "start" | "end";
  searchable?: boolean | "auto";
  /** Renders the trigger. Spread `trigger` onto the button element. */
  children: (trigger: {
    ref: (node: HTMLElement | null) => void;
    onClick: () => void;
    "aria-haspopup": "menu";
    "aria-expanded": boolean;
  }) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const close = useCallback(() => setOpen(false), []);

  return (
    <>
      {children({
        ref: setAnchor,
        onClick: () => setOpen((current) => !current),
        "aria-haspopup": "menu",
        "aria-expanded": open,
      })}
      {open && (
        <Menu
          anchor={anchor}
          items={items}
          onClose={close}
          label={label}
          title={title}
          align={align}
          searchable={searchable}
        />
      )}
    </>
  );
}
