"use client";

/**
 * Edge-anchored panels: a bottom sheet and a side drawer.
 *
 * These are the mobile form of the two sidebars. On a phone the three-pane shell
 * cannot lay out in flow — a 240px sidebar on a 390px viewport leaves 150px for
 * the conversation *and* the composer — so the panes become surfaces that overlay
 * the conversation and are dismissed.
 *
 * # Why a bottom sheet for files, and a side drawer for sessions
 *
 * They answer different questions. Sessions are navigation: a left drawer is where
 * a phone user reaches for it, and it matches the hamburger that opens it. Files
 * are inspection *of the thing on screen* — the sheet keeps the conversation
 * visible above it, and a vertical drag is the gesture for "show me more / put it
 * away". This is the split Claude and ChatGPT's iOS apps use, and it is why the
 * file sheet is drag-dismissable while the drawer is not: a horizontal drag on a
 * drawer competes with the browser's own back-swipe gesture on iOS.
 *
 * # Dragging
 *
 * Pointer events, not touch events, so the same code path works when a desktop
 * browser is emulating a phone — which is how this is tested. Only downward drag
 * is honoured, and only past a distance *or* a velocity threshold: a slow short
 * drag springs back, a fast flick dismisses even if short, which is what makes the
 * gesture feel like it has weight. During a drag the transform is written directly
 * to the node rather than held in state; a state write per `pointermove` would
 * re-render the whole subtree at touch-sampling rate.
 */

import { useT } from "@/lib/i18n";
import { useCallback, useRef, type ReactNode } from "react";
import { useDialog } from "@/lib/use-dialog";
import { IconButton } from "@/components/ui/button";
import { XIcon } from "@/components/icons";

/** Drag distance past which release dismisses, in px. */
const DISMISS_DISTANCE = 96;
/** Downward velocity past which release dismisses, in px/ms. */
const DISMISS_VELOCITY = 0.55;

/**
 * A panel anchored to the bottom edge.
 *
 * `max-h-[85dvh]`, not `85vh`: `vh` measures the viewport as though the browser's
 * toolbars were hidden, so a `vh`-sized sheet extends behind them and its own
 * footer becomes unreachable.
 */
export function Sheet({
  title,
  onClose,
  children,
  /** Rendered in the header, right of the title. */
  actions,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  actions?: ReactNode;
}) {
  const t = useT();
  const { ref, onKeyDown } = useDialog<HTMLDivElement>({ onClose });
  const panelRef = useRef<HTMLDivElement>(null);
  const drag = useRef<{ startY: number; startTime: number; offset: number } | null>(null);

  const onPointerDown = useCallback((event: React.PointerEvent) => {
    // Ignore secondary buttons, and let a pinch-zoom through untouched.
    if (event.button !== 0 || !event.isPrimary) return;
    drag.current = { startY: event.clientY, startTime: performance.now(), offset: 0 };
    // Capture so the gesture survives the pointer leaving the handle, which it
    // does immediately on any real drag.
    event.currentTarget.setPointerCapture(event.pointerId);
  }, []);

  const onPointerMove = useCallback((event: React.PointerEvent) => {
    const state = drag.current;
    const node = panelRef.current;
    if (!state || !node) return;
    // Downward only. Rubber-banding a sheet upward past its own top edge exposes
    // the canvas behind it, which reads as a rendering fault.
    const offset = Math.max(0, event.clientY - state.startY);
    state.offset = offset;
    node.style.transform = `translateY(${offset}px)`;
    // The scrim thins as the sheet leaves, so the gesture feels connected to it.
    node.style.setProperty("--drag-progress", String(1 - Math.min(offset / 320, 0.75)));
  }, []);

  const onPointerUp = useCallback(
    (event: React.PointerEvent) => {
      const state = drag.current;
      const node = panelRef.current;
      drag.current = null;
      if (!state || !node) return;
      event.currentTarget.releasePointerCapture(event.pointerId);

      const elapsed = Math.max(1, performance.now() - state.startTime);
      const velocity = state.offset / elapsed;
      if (state.offset > DISMISS_DISTANCE || velocity > DISMISS_VELOCITY) {
        onClose();
        return;
      }
      // Spring back. The transition is set only for the return trip, so it does
      // not lag the finger on the way down.
      node.style.transition = "transform 180ms cubic-bezier(0.32, 0.72, 0, 1)";
      node.style.transform = "";
      node.style.removeProperty("--drag-progress");
      const clear = () => {
        node.style.transition = "";
        node.removeEventListener("transitionend", clear);
      };
      node.addEventListener("transitionend", clear);
    },
    [onClose],
  );

  return (
    <div className="fixed inset-0 z-40 flex flex-col justify-end">
      <button
        type="button"
        aria-label={t("action.close")}
        onClick={onClose}
        className="animate-scrim absolute inset-0 cursor-default bg-ink/35 backdrop-blur-[1px]"
        tabIndex={-1}
      />
      <div
        ref={panelRef}
        className="animate-sheet-up relative flex max-h-[85dvh] min-h-0 flex-col overflow-hidden rounded-t-2xl border-t border-line bg-raised shadow-2xl"
      >
        <div
          ref={ref}
          role="dialog"
          aria-modal="true"
          aria-label={title}
          tabIndex={-1}
          onKeyDown={onKeyDown}
          className="flex min-h-0 flex-1 flex-col outline-none"
        >
          {/*
            The drag surface is the header, not just the grab bar: a 4px-tall
            target is unhittable, and a user who has decided to put the sheet away
            aims at its top edge. `touch-none` stops the browser from claiming the
            gesture as a scroll before the handler sees it.
          */}
          <div
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={onPointerUp}
            className="shrink-0 cursor-grab touch-none active:cursor-grabbing"
          >
            <div className="flex justify-center pt-2 pb-1">
              <span aria-hidden="true" className="h-1 w-9 rounded-full bg-line-strong" />
            </div>
            <div className="flex items-center gap-2 px-3 pb-2">
              <h2 className="min-w-0 flex-1 truncate text-sm font-semibold">{title}</h2>
              {actions}
              <IconButton label="Close" onClick={onClose}>
                <XIcon />
              </IconButton>
            </div>
          </div>
          {children}
        </div>
      </div>
    </div>
  );
}

/**
 * A full-height panel anchored to the left edge.
 *
 * Not drag-dismissable, deliberately: a rightward drag from the left edge is iOS
 * Safari's own back-navigation gesture, and competing with it means the drawer
 * sometimes closes and sometimes navigates the history. Tap-the-scrim and the
 * close button are unambiguous, and Escape works for a keyboard.
 */
export function Drawer({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const t = useT();
  const { ref, onKeyDown } = useDialog<HTMLDivElement>({ onClose });

  return (
    <div className="fixed inset-0 z-40 flex">
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        // `max-w-[85vw]` so the conversation stays visible at the edge: a drawer
        // that covers everything is indistinguishable from a navigation, and the
        // sliver of context is what tells the user they can dismiss it.
        className="animate-drawer-left relative flex w-72 max-w-[85vw] flex-col border-r border-line bg-sunken pt-safe outline-none"
      >
        {children}
      </div>
      <button
        type="button"
        aria-label={t("action.close")}
        onClick={onClose}
        className="animate-scrim flex-1 cursor-default bg-ink/35 backdrop-blur-[1px]"
        tabIndex={-1}
      />
    </div>
  );
}
