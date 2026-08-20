"use client";

/**
 * Right-click menu.
 *
 * Hand-rolled rather than pulled from a component library: the behaviours that
 * make a context menu correct are focus containment, arrow navigation, Escape,
 * and viewport clamping, and all four fit here in less code than the dependency
 * would add to the first-load budget. The `menu`/`menuitem` roles carry the
 * semantics a screen reader needs, so nothing is lost by not using a library.
 */

import { useEffect, useRef, useState } from "react";

export interface MenuItem {
  label: string;
  onSelect: () => void;
  /** Renders as a dimmed, unfocusable row — used for actions that exist but do
   *  not apply to the current target. */
  disabled?: boolean;
  /** Draws a separator above this item. */
  separated?: boolean;
}

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
  const enabled = items.filter((item) => !item.disabled);
  const [activeIndex, setActiveIndex] = useState(0);

  // Clamp inside the viewport after mount, when the real size is known. Doing
  // this from the click coordinates alone would guess at the height and still
  // clip a menu opened near the bottom edge.
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const rect = node.getBoundingClientRect();
    const left = Math.min(x, window.innerWidth - rect.width - 8);
    const top = Math.min(y, window.innerHeight - rect.height - 8);
    setPosition({ left: Math.max(8, left), top: Math.max(8, top) });
    node.focus();
  }, [x, y]);

  useEffect(() => {
    // Pointer-down, not click: a menu that survives until mouseup can be
    // dismissed by a drag that started elsewhere, which feels stuck.
    function onPointerDown(event: PointerEvent) {
      if (!ref.current?.contains(event.target as Node)) onClose();
    }
    // Scrolling detaches the menu from what it was anchored to.
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", onClose, true);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", onClose, true);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  function choose(item: MenuItem) {
    onClose();
    item.onSelect();
  }

  return (
    <div
      ref={ref}
      role="menu"
      aria-label={label}
      tabIndex={-1}
      style={{ left: position.left, top: position.top }}
      onKeyDown={(event) => {
        // Escape is handled here and stopped, so it closes the menu rather than
        // travelling up to the shell and closing an overlay behind it.
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
          return;
        }
        if (event.key === "ArrowDown") {
          event.preventDefault();
          setActiveIndex((index) => (index + 1) % enabled.length);
        } else if (event.key === "ArrowUp") {
          event.preventDefault();
          setActiveIndex((index) => (index - 1 + enabled.length) % enabled.length);
        } else if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          const item = enabled[activeIndex];
          if (item) choose(item);
        }
      }}
      className="fixed z-50 min-w-44 overflow-hidden rounded-md border border-gray-200 bg-white py-1 text-xs shadow-lg outline-none"
    >
      {items.map((item, index) => {
        const enabledIndex = enabled.indexOf(item);
        const active = enabledIndex === activeIndex;
        return (
          <div key={item.label}>
            {item.separated && <div role="separator" className="my-1 border-t border-gray-100" />}
            <button
              type="button"
              role="menuitem"
              disabled={item.disabled}
              // The menu itself holds focus and manages a virtual cursor, so
              // rows are not tab stops; arrow keys move `activeIndex` instead.
              tabIndex={-1}
              onMouseEnter={() => enabledIndex >= 0 && setActiveIndex(enabledIndex)}
              onClick={() => choose(item)}
              className={`block w-full px-3 py-1.5 text-left ${
                item.disabled
                  ? "cursor-default text-gray-500"
                  : active
                    ? "bg-gray-100 text-gray-900"
                    : "text-gray-700"
              }`}
            >
              {item.label}
            </button>
            {index === items.length - 1 && null}
          </div>
        );
      })}
    </div>
  );
}
