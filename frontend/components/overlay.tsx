"use client";

/**
 * Modal shell shared by search, the command palette, and settings.
 *
 * Focus handling is the reason this is one component rather than three: a modal
 * that does not trap focus leaves a keyboard user tabbing through the page behind
 * it, and one that does not restore focus on close drops them at the top of the
 * document. Both are WCAG 2.4.3 failures and both are easy to get subtly wrong.
 * They now live in `lib/use-dialog.ts`, shared with the mobile drawer and sheet —
 * this component had its own copy, and `CommandDialog` had a third that differed in
 * ways only a keyboard user would find.
 *
 * `<dialog>` is not used: `showModal()` gives focus trapping and a top-layer
 * backdrop for free, but it also renders outside React's tree for stacking
 * purposes and its `::backdrop` cannot be styled by Tailwind utilities.
 *
 * # On a phone, a picker is full-height
 *
 * A dialog capped at `max-h-[80vh]` and offset `pt-[10vh]` is a small box floating
 * in the middle of a phone screen with its list clipped to a few rows. On mobile
 * the picker variants therefore fill the viewport: the search overlay, the palette,
 * and the history browser are *the* task while they are open, and a list is what
 * they are made of. `dvh`, not `vh`, so the browser's collapsing toolbars do not
 * push the footer out of reach.
 */

import { useId, type ReactNode } from "react";
import { useDialog } from "@/lib/use-dialog";

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
  const titleId = useId();
  // `autoFocus: false` — the pickers all carry an `autoFocus` input, and taking
  // focus to the container first would move the caret out of it.
  const { ref, onKeyDown } = useDialog<HTMLDivElement>({ onClose, autoFocus: false });

  // No body-scroll lock. This used to set `body.style.overflow = "hidden"`, which
  // was both ineffective against iOS rubber-banding and is now redundant: the
  // document is pinned in `globals.css` and the shell owns all scrolling. Two
  // mechanisms for one invariant is how a stale inline style gets left behind when
  // two dialogs unmount out of order.

  const viewer = size === "viewer" || size === "viewer-full";

  return (
    <div
      // A picker sits below the top of the viewport; the viewer is centred and
      // near-full-bleed, because vertical space is exactly what a spreadsheet or
      // a rendered page needs. On a phone both fill it: there is no spare room to
      // frame a dialog in, and the inset would only clip the content.
      className={`fixed inset-0 z-40 flex justify-center bg-ink/35 backdrop-blur-[1px] max-md:p-0 md:p-4 ${
        viewer ? "items-center" : "items-start md:pt-[10vh]"
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
        // `dvh` throughout, never `vh`: `vh` measures as though the browser's
        // toolbars were hidden, so a `92vh` dialog on a phone extends behind them
        // and its own footer — the confirm button, the paging control — is
        // unreachable.
        //
        // On mobile every variant is full-screen. `min-w-[32rem]` on the `viewer`
        // variant is wider than a phone on its own, so it must not apply there.
        className={`flex flex-col overflow-hidden border-line bg-raised shadow-2xl outline-none max-md:h-dvh max-md:w-full max-md:pt-safe md:rounded-xl md:border ${
          size === "viewer"
            ? "md:h-fit md:max-h-[92dvh] md:w-fit md:min-w-[32rem] md:max-w-[95vw]"
            : size === "viewer-full"
              ? "md:h-[92dvh] md:w-full md:max-w-[95vw]"
              : `md:w-full md:max-h-[80dvh] ${size === "panel" ? "md:max-w-3xl" : "md:max-w-xl"}`
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
  // Stateless on purpose: the caller owns `active`, because in all four pickers it
  // is one field of a state object that also holds the query — which is what makes
  // "a query edit resets the cursor" a single update rather than an effect. This
  // hook previously also kept an `activeRef` and exported a `reset`, neither of
  // which any caller used, so the cursor existed in two places and only one of them
  // was read.
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

  return { onKeyDown };
}

/** Scroll the active row into view without yanking the whole page. */
export function scrollActiveIntoView(container: HTMLElement | null, index: number) {
  const row = container?.querySelector<HTMLElement>(`[data-index="${index}"]`);
  row?.scrollIntoView({ block: "nearest" });
}
