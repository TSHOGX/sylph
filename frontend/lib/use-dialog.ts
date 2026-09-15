"use client";

/**
 * The behaviour every modal surface needs: trap focus, restore it on close, and
 * close on Escape.
 *
 * # Why it is a hook
 *
 * Three surfaces implemented this independently — `Overlay`, `CommandDialog`, and
 * the context menu — and they disagreed in ways that are invisible until someone
 * uses a keyboard. `CommandDialog` queried `"button, select, input, textarea,
 * [href]"`, which matches a *disabled* button and so could park focus on a control
 * that cannot be activated. `Overlay` filtered on `offsetParent`, which correctly
 * skips hidden elements. Neither stopped Escape from also reaching the shell's
 * document listener, so closing a dialog could close the overlay behind it too.
 *
 * These are WCAG 2.4.3 and 2.1.2 obligations. One implementation is one chance to
 * get them wrong, and the drawers and sheets added for mobile would otherwise have
 * been a fourth and fifth copy.
 *
 * # What it deliberately does not do
 *
 * It does not lock body scroll. It used to, via `body.style.overflow = "hidden"`,
 * which was both ineffective against iOS rubber-banding and now redundant: the
 * document is pinned globally in `globals.css` and the shell owns all scrolling.
 * A lock here would be a second mechanism for the same invariant, and the kind
 * that leaks a stale style when two dialogs unmount out of order.
 */

import { useCallback, useEffect, useRef, type KeyboardEvent, type RefObject } from "react";

/**
 * Tabbable descendants.
 *
 * `:not([disabled])` matters: a disabled control is not tabbable, so including it
 * would let the trap hand focus to something inert at the edge of the cycle.
 */
export const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Visible, focusable children in document order. */
function tabbable(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    // `offsetParent === null` catches `display: none` and `hidden` subtrees. The
    // `activeElement` exemption keeps the currently focused element in the cycle
    // even if it is inside a container being animated to zero size.
    (element) => element.offsetParent !== null || element === document.activeElement,
  );
}

export interface DialogOptions {
  enabled?: boolean;
  onClose: () => void;
  /**
   * Move focus to the first tabbable control on mount.
   *
   * Off for a surface whose own content claims focus — a picker with an `autoFocus`
   * input already has it, and taking it again would move the caret to a button.
   *
   * This does **not** mean "leave focus alone". Focus still lands on the container
   * when nothing inside the surface has it; see the effect below for why that is
   * mandatory rather than a nicety.
   */
  autoFocus?: boolean;
}

export interface Dialog<T extends HTMLElement> {
  /** Attach to the surface root. */
  ref: RefObject<T | null>;
  /** Attach to the surface root; handles Escape and Tab. */
  onKeyDown: (event: KeyboardEvent) => void;
}

export function useDialog<T extends HTMLElement>({
  onClose,
  autoFocus = true,
  enabled = true,
}: DialogOptions): Dialog<T> {
  const ref = useRef<T | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!enabled) return;
    // Captured before focus moves, so it can be handed back.
    openerRef.current = document.activeElement as HTMLElement | null;

    const node = ref.current;
    if (node) {
      if (autoFocus) {
        // The container itself when there is nothing focusable inside, so the first
        // Tab starts within the surface rather than at the top of the document.
        (tabbable(node)[0] ?? node).focus();
      } else if (!node.contains(document.activeElement)) {
        // Focus the container even when `autoFocus` is off, if nothing inside the
        // surface took it.
        //
        // This is not cosmetic. `onKeyDown` is a React handler on the container, so
        // it only sees keys dispatched at or below it — and Escape is what closes
        // the dialog (WCAG 2.1.2). With focus still on `<body>`, the key never
        // arrives and the surface cannot be dismissed by keyboard at all. That is
        // exactly what happened to the full-screen file viewer: it has no
        // `autoFocus` control of its own, so it opened unclosable.
        //
        // Guarded on `contains`, so a picker whose `autoFocus` input has already
        // claimed focus keeps it and the caret does not move.
        node.focus();
      }
    }

    return () => {
      const opener = openerRef.current;
      // `isConnected` guards the case where the action that closed this surface
      // also unmounted its opener. Focusing a detached node silently drops focus
      // to `<body>`, which is worse than leaving it where it is.
      if (opener?.isConnected) opener.focus();
    };
  }, [autoFocus, enabled]);

  const onKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (!enabled) return;
      if (event.key === "Escape") {
        // Stopped here so it closes *this* surface only. Without this the shell's
        // document-level handler also fires and closes whatever is behind.
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;

      const node = ref.current;
      if (!node) return;
      const items = tabbable(node);
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
    },
    [onClose, enabled],
  );

  return { ref, onKeyDown };
}
