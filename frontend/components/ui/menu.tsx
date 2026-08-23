"use client";

/**
 * Menus: one behaviour, three ways of being positioned.
 *
 * - `ContextMenu` is anchored to a point (a right-click).
 * - `Menu` is anchored to a trigger element (a dropdown), and becomes a bottom
 *   sheet on a phone, where a 200px popover pinned to a 32px button is both hard
 *   to hit and prone to landing under the on-screen keyboard.
 * - `MenuItems` is the shared list: roles, arrow navigation, Escape, checkmarks.
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
 * # Roles
 *
 * `menu`/`menuitem`, with `aria-activedescendant` for the virtual cursor and
 * `menuitemradio` + `aria-checked` when a group of items is a single choice. The
 * container holds DOM focus and rows are not tab stops — the pattern ARIA
 * prescribes, and the only one where arrow keys and typing do not fight.
 */

import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useIsMobile } from "@/lib/viewport";
import { Sheet } from "@/components/ui/sheet";
import { CheckIcon } from "@/components/icons";

export interface MenuItem {
  label: string;
  onSelect: () => void;
  /** Leading icon. Decorative; the label is the accessible name. */
  icon?: ReactNode;
  /** Trailing hint — a shortcut, or a value. */
  hint?: string;
  /** Renders as a dimmed, unfocusable row: the action exists but does not apply. */
  disabled?: boolean;
  /** Draws a separator above this row. */
  separated?: boolean;
  /**
   * Marks this row as the current choice in a single-select group, which also
   * switches its role to `menuitemradio`. `undefined` for a plain action, so an
   * action is never announced as an unchecked option.
   */
  checked?: boolean;
  /** Destructive: stop, discard, delete. */
  danger?: boolean;
}

/* ── Shared list ──────────────────────────────────────────────────────────── */

function MenuItems({
  items,
  activeIndex,
  setActiveIndex,
  onChoose,
  rowId,
}: {
  items: MenuItem[];
  activeIndex: number;
  setActiveIndex: (index: number) => void;
  onChoose: (item: MenuItem) => void;
  rowId: (index: number) => string;
}) {
  const enabled = items.filter((item) => !item.disabled);
  // Reserve the check column only when something in this menu is checkable, so an
  // action menu is not indented for a state it never shows.
  const checkable = items.some((item) => item.checked !== undefined);

  return (
    <>
      {items.map((item) => {
        const enabledIndex = enabled.indexOf(item);
        const active = enabledIndex === activeIndex && enabledIndex >= 0;
        return (
          <div key={item.label}>
            {item.separated && (
              <div role="separator" className="my-1 border-t border-line" />
            )}
            <button
              type="button"
              id={enabledIndex >= 0 ? rowId(enabledIndex) : undefined}
              role={item.checked === undefined ? "menuitem" : "menuitemradio"}
              aria-checked={item.checked}
              disabled={item.disabled}
              // The container owns focus and moves a virtual cursor instead.
              tabIndex={-1}
              onMouseEnter={() => enabledIndex >= 0 && setActiveIndex(enabledIndex)}
              onClick={() => onChoose(item)}
              className={`tap-row flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs ${
                item.disabled
                  ? "cursor-default text-ink-faint"
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
                <span className="w-4 shrink-0 text-ink-muted">
                  {item.checked && <CheckIcon size={14} />}
                </span>
              )}
              {item.icon && <span className="shrink-0 text-ink-muted">{item.icon}</span>}
              <span className="min-w-0 flex-1 truncate">{item.label}</span>
              {item.hint && (
                <span className="shrink-0 font-mono text-3xs text-ink-muted">{item.hint}</span>
              )}
            </button>
          </div>
        );
      })}
    </>
  );
}

/**
 * Arrow navigation over the enabled subset.
 *
 * Wraps at both ends, which is what a menu does everywhere; Home and End are also
 * bound because a long menu is faster to traverse that way.
 */
function useMenuKeys(count: number, onClose: () => void, onCommit: (index: number) => void) {
  const [activeIndex, setActiveIndex] = useState(0);

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent) => {
      if (event.key === "Escape") {
        // Stopped so it closes this menu only, not an overlay behind it.
        event.stopPropagation();
        onClose();
        return;
      }
      if (count === 0) return;
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActiveIndex((index) => (index + 1) % count);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        setActiveIndex((index) => (index - 1 + count) % count);
      } else if (event.key === "Home") {
        event.preventDefault();
        setActiveIndex(0);
      } else if (event.key === "End") {
        event.preventDefault();
        setActiveIndex(count - 1);
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        onCommit(activeIndex);
      }
    },
    [count, activeIndex, onClose, onCommit],
  );

  return { activeIndex, setActiveIndex, onKeyDown };
}

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
    // Pointer-down rather than click: a menu that survives until mouseup can be
    // dismissed by a drag that began elsewhere, which feels stuck.
    function onPointerDown(event: PointerEvent) {
      if (!ref.current?.contains(event.target as Node)) onClose();
    }
    // Scrolling detaches the menu from whatever it was anchored to. Capture phase,
    // because scroll does not bubble from the pane that actually scrolled.
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", onClose, true);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", onClose, true);
      window.removeEventListener("resize", onClose);
    };
  }, [ref, onClose, enabled]);
}

const PANEL =
  "z-50 min-w-48 max-w-[min(20rem,calc(100vw-1rem))] overflow-hidden rounded-xl border border-line bg-raised py-1 shadow-xl outline-none";

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
  const baseId = useId();
  const enabled = items.filter((item) => !item.disabled);

  const choose = useCallback(
    (item: MenuItem) => {
      onClose();
      item.onSelect();
    },
    [onClose],
  );

  const { activeIndex, setActiveIndex, onKeyDown } = useMenuKeys(
    enabled.length,
    onClose,
    (index) => {
      const item = enabled[index];
      if (item) choose(item);
    },
  );

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
      aria-activedescendant={enabled.length > 0 ? `${baseId}-${activeIndex}` : undefined}
      tabIndex={-1}
      style={{ left: position.left, top: position.top }}
      onKeyDown={onKeyDown}
      className={`fixed ${PANEL}`}
    >
      <MenuItems
        items={items}
        activeIndex={activeIndex}
        setActiveIndex={setActiveIndex}
        onChoose={choose}
        rowId={(index) => `${baseId}-${index}`}
      />
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
}) {
  const isMobile = useIsMobile();
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const baseId = useId();
  const enabled = items.filter((item) => !item.disabled);

  const choose = useCallback(
    (item: MenuItem) => {
      onClose();
      item.onSelect();
    },
    [onClose],
  );

  const { activeIndex, setActiveIndex, onKeyDown } = useMenuKeys(
    enabled.length,
    onClose,
    (index) => {
      const item = enabled[index];
      if (item) choose(item);
    },
  );

  // Measured after mount for the same reason as the context menu, plus one more:
  // the composer's controls sit at the bottom of the viewport, so a menu placed
  // below its trigger would open off-screen. It flips above when it does not fit.
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
   */
  const focusedRef = useRef(false);
  useEffect(() => {
    if (isMobile || position === null || focusedRef.current) return;
    focusedRef.current = true;
    ref.current?.focus();
  }, [position, isMobile]);

  /**
   * Hand focus back to the trigger when the menu closes.
   *
   * A menu that takes focus and does not return it drops a keyboard user at the top
   * of the document (WCAG 2.4.3). It also breaks the *next* surface in a chain: an
   * item that opens a dialog — "Run a command..." — has the menu unmount in the same
   * commit the dialog mounts, and the dialog captures `document.activeElement` as
   * the opener it will later restore to. With focus on a disappearing menu, that
   * capture is `<body>`, so closing the dialog stranded focus there.
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

  const list = (
    <MenuItems
      items={items}
      activeIndex={activeIndex}
      setActiveIndex={setActiveIndex}
      onChoose={choose}
      rowId={(index) => `${baseId}-${index}`}
    />
  );

  // On a phone the same items become a bottom sheet: reachable one-handed, rows at
  // full touch height, and never clipped by the on-screen keyboard.
  if (isMobile) {
    return (
      <Sheet title={title ?? label} onClose={onClose}>
        <div role="menu" aria-label={label} className="pane min-h-0 flex-1 pb-safe">
          {list}
        </div>
      </Sheet>
    );
  }

  return (
    <div
      ref={ref}
      role="menu"
      aria-label={label}
      aria-activedescendant={enabled.length > 0 ? `${baseId}-${activeIndex}` : undefined}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      // Hidden until measured, so it does not flash at the wrong place for a frame.
      style={position ?? { left: -9999, top: 0 }}
      className={`fixed ${PANEL} ${position ? "" : "invisible"}`}
    >
      {list}
    </div>
  );
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
          onClose={() => setOpen(false)}
          label={label}
          title={title}
          align={align}
        />
      )}
    </>
  );
}
