"use client";

/**
 * Modal shell shared by search, the command palette, and settings.
 *
 * Focus handling is the reason this is one component rather than three: a modal
 * that does not trap focus leaves a keyboard user tabbing through the page behind
 * it, and one that does not restore focus on close drops them at the top of the
 * document. Both are WCAG 2.4.3 failures and both are easy to get subtly wrong,
 * so they are implemented once.
 *
 * `<dialog>` is not used: `showModal()` gives focus trapping and a top-layer
 * backdrop for free, but it also renders outside React's tree for stacking
 * purposes and its `::backdrop` cannot be styled by Tailwind utilities. The trap
 * below is short enough that the trade is not worth it.
 */

import { useEffect, useId, useRef, type ReactNode } from "react";

export function Overlay({
  title,
  onClose,
  children,
  /**
   * Wider layout for settings, which is a panel rather than a picker. The two
   * `viewer` variants are for the full-screen file viewer, where the content is
   * the point and the chrome should get out of its way — see the sizing comment
   * below for why one hugs its content and the other fills the viewport.
   */
  size = "picker",
  describedBy,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  size?: "picker" | "panel" | "viewer" | "viewer-full";
  describedBy?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const titleId = useId();

  useEffect(() => {
    // Remember who opened this before moving focus, so it can be handed back.
    openerRef.current = document.activeElement as HTMLElement | null;

    const node = ref.current;
    // Focus the first control, or the container itself when there is none, so
    // the first Tab stays inside the modal rather than starting from the page.
    const focusable = node?.querySelector<HTMLElement>(FOCUSABLE);
    (focusable ?? node)?.focus();

    return () => {
      // `isConnected` guards the case where the opener was itself unmounted by
      // the action that closed the modal; focusing a detached node silently
      // moves focus to <body>, which is worse than leaving it alone.
      const opener = openerRef.current;
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    // Background scroll is locked because a scrolling page behind a modal makes
    // the modal look detached and lets a trackpad move content the user cannot
    // interact with.
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = previous;
    };
  }, []);

  function onKeyDown(event: React.KeyboardEvent) {
    if (event.key === "Escape") {
      // Stopped here so it closes this modal only, and does not also reach the
      // shell's document-level handler.
      event.stopPropagation();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;

    const node = ref.current;
    if (!node) return;
    const items = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (element) => element.offsetParent !== null || element === document.activeElement,
    );
    if (items.length === 0) return;

    const first = items[0];
    const last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  return (
    <div
      // A picker sits below the top of the viewport; the viewer is centred and
      // near-full-bleed, because vertical space is exactly what a spreadsheet or
      // a rendered page needs.
      className={`fixed inset-0 z-40 flex justify-center bg-gray-900/30 p-4 backdrop-blur-[1px] ${
        size === "viewer" || size === "viewer-full"
          ? "items-center"
          : "items-start pt-[10vh]"
      }`}
      // The backdrop closes on click, which every picker convention supports.
      // `onMouseDown` on the backdrop only, so a drag that ends outside a text
      // selection inside the dialog does not dismiss it.
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={describedBy}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        // Two viewer sizings, because content types differ in a way one rule
        // cannot cover:
        //
        // `viewer` hugs its content and stops at the viewport. A seven-row table
        // in a fixed 92vh x 95vw box reads as broken even though nothing is,
        // while a 200-row sheet still fills the screen because its own height
        // asks for it. The `min-w` floor keeps the dialog from snapping narrow to
        // wide as the body loads.
        //
        // `viewer-full` claims the space outright, for content with no intrinsic
        // size to measure — an iframe reports none, so a rendered page or a PDF
        // would collapse to the floor under `w-fit`. Those are also the formats
        // authored *for* a viewport, so filling one is what they expect.
        className={`flex flex-col overflow-hidden rounded-xl border border-gray-200 bg-white shadow-2xl outline-none ${
          size === "viewer"
            ? "h-fit max-h-[92vh] w-fit min-w-[32rem] max-w-[95vw]"
            : size === "viewer-full"
              ? "h-[92vh] w-full max-w-[95vw]"
              : `w-full max-h-[80vh] ${size === "panel" ? "max-w-3xl" : "max-w-xl"}`
        }`}
      >
        <h2 id={titleId} className="sr-only">
          {title}
        </h2>
        {children}
      </div>
    </div>
  );
}

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * The list behaviour every picker needs: arrow keys move a virtual cursor, Enter
 * commits, and the input keeps focus throughout.
 *
 * Focus stays on the input rather than moving to the active row, which is why
 * rows are marked with `aria-selected` and the input carries
 * `aria-activedescendant` — the pattern ARIA prescribes for a combobox, and the
 * only one where typing and navigating do not fight over focus.
 */
export function useListNavigation(count: number, onCommit: (index: number) => void) {
  const activeRef = useRef(0);

  function reset() {
    activeRef.current = 0;
  }

  function onKeyDown(event: React.KeyboardEvent, active: number, setActive: (n: number) => void) {
    if (count === 0) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((active + 1) % count);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((active - 1 + count) % count);
    } else if (event.key === "Home") {
      event.preventDefault();
      setActive(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setActive(count - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      onCommit(active);
    }
  }

  return { onKeyDown, reset };
}

/** Scroll the active row into view without yanking the whole page. */
export function scrollActiveIntoView(container: HTMLElement | null, index: number) {
  const row = container?.querySelector<HTMLElement>(`[data-index="${index}"]`);
  row?.scrollIntoView({ block: "nearest" });
}
