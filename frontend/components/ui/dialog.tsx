"use client";

/**
 * Small decision dialogs: confirm a destructive action, ask for one short value.
 *
 * # Why not `window.confirm`
 *
 * It was the obvious first answer for "confirm delete", and it is wrong on every
 * axis that matters here. It blocks the main thread, so the SSE stream driving
 * the timeline stalls while it is up. It cannot be styled, so it announces the
 * page as a web page in the middle of an interface built to read as an
 * application — and on iOS it names the origin ("localhost:3000 says"), which is
 * exactly the "assembled from different kits" quality the design pass removed
 * from the composer's `<select>` elements. It cannot be tested by Playwright
 * without a dialog handler that swallows real failures too. And it offers only
 * OK/Cancel, so the *name* of the action is not on the button the user presses,
 * which is the one thing that makes a destructive confirm safe to read quickly.
 *
 * # Why `AlertDialog` and `PromptDialog` are separate
 *
 * They differ in what holds focus on open, and that is not cosmetic. A confirm
 * focuses its *cancel* affordance, so a stray Enter on an arriving dialog does
 * not delete anything; a prompt focuses its input, because typing is the whole
 * task. One component with a flag would put that distinction behind a boolean
 * that a caller has to know to set, and the safe default is the one nobody sets.
 *
 * Both reuse `useDialog` for the focus trap, Escape, and focus restore, so this
 * module does not become a fourth implementation of the obligations documented
 * there.
 */

import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useDialog } from "@/lib/use-dialog";
import { Button } from "@/components/ui/button";

/**
 * The shell: scrim, panel, title, description, and a footer of actions.
 *
 * Centred rather than sheet-like on a phone. A decision dialog is not a picker —
 * it has two buttons and three lines of text, and filling a viewport with that
 * makes a small question look like a page change. It also stays reachable above
 * the on-screen keyboard, which a bottom sheet does not.
 */
function DialogShell({
  title,
  description,
  onClose,
  autoFocus,
  children,
  footer,
}: {
  title: string;
  description?: ReactNode;
  onClose: () => void;
  autoFocus: boolean;
  children?: ReactNode;
  footer: ReactNode;
}) {
  const titleId = useId();
  const descriptionId = useId();
  const { ref, onKeyDown } = useDialog<HTMLDivElement>({ onClose, autoFocus });

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-ink/40 p-4 backdrop-blur-[1px]"
      // `onMouseDown` on the scrim only, so a text selection that ends outside
      // the panel does not dismiss it mid-drag.
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={ref}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        className="flex w-full max-w-sm flex-col gap-3 rounded-xl border border-line bg-raised p-4 shadow-2xl outline-none"
      >
        <div className="flex flex-col gap-1">
          <h2 id={titleId} className="text-xs font-semibold text-ink">
            {title}
          </h2>
          {description && (
            <div id={descriptionId} className="text-2xs leading-relaxed text-ink-soft">
              {description}
            </div>
          )}
        </div>
        {children}
        {/*
          Normal DOM order, not `flex-row-reverse`.

          Reversing was the first attempt, so that the affirmative button could be
          written first and still land on the right. It puts the *destructive*
          button first in the tab order, which `useDialog` then focuses on open —
          so an Enter keystroke in flight when a delete dialog appeared would
          confirm it. The E2E spec caught exactly that. Ordering the DOM as
          Cancel-then-confirm gives both properties at once: Cancel is the first
          tab stop, and `ml-auto` on the group keeps the pair right-aligned with
          the affirmative button last, where macOS and Windows both put it.
        */}
        <div className="flex items-center gap-2">{footer}</div>
      </div>
    </div>
  );
}

export function AlertDialog({
  title,
  description,
  confirmLabel,
  danger = false,
  busy = false,
  error,
  onConfirm,
  onCancel,
}: {
  title: string;
  description?: ReactNode;
  /**
   * Names the action, never "OK".
   *
   * "Delete" on the button the user presses is what lets them confirm without
   * re-reading the sentence above it — and what makes an accidental Enter on the
   * wrong dialog recoverable, because the button says what it will do.
   */
  confirmLabel: string;
  danger?: boolean;
  busy?: boolean;
  /** A failed attempt, shown in place rather than closing the dialog. */
  error?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  // Focus lands on Cancel: `useDialog`'s first-tabbable rule plus Cancel being
  // first in the DOM. Deliberate — an Enter keystroke in flight when the dialog
  // opens must not confirm a deletion.
  return (
    <DialogShell
      title={title}
      description={description}
      onClose={onCancel}
      autoFocus
      footer={
        <>
          {error && (
            <p role="alert" className="min-w-0 flex-1 text-3xs text-danger">
              {error}
            </p>
          )}
          <Button variant="ghost" className="ml-auto" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm} disabled={busy}>
            {busy ? "Working..." : confirmLabel}
          </Button>
        </>
      }
    />
  );
}

export function PromptDialog({
  title,
  description,
  label,
  confirmLabel,
  initialValue = "",
  placeholder,
  busy = false,
  error,
  onSubmit,
  onCancel,
}: {
  title: string;
  description?: ReactNode;
  /** The input's accessible name. Visible, because a bare box is a guess. */
  label: string;
  confirmLabel: string;
  initialValue?: string;
  placeholder?: string;
  busy?: boolean;
  error?: string;
  onSubmit: (value: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initialValue);
  const inputRef = useRef<HTMLInputElement>(null);
  const inputId = useId();

  // `autoFocus: false` on the shell, then focus the input here. The same split
  // `Overlay` makes for its pickers: letting the trap take the first tabbable
  // control would park focus on a button while the user is trying to type.
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const trimmed = value.trim();

  return (
    <DialogShell
      title={title}
      description={description}
      onClose={onCancel}
      autoFocus={false}
      footer={
        <>
          {error && (
            <p role="alert" className="min-w-0 flex-1 text-3xs text-danger">
              {error}
            </p>
          )}
          <Button variant="ghost" className="ml-auto" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={() => onSubmit(trimmed)}
            disabled={busy || trimmed.length === 0}
          >
            {busy ? "Working..." : confirmLabel}
          </Button>
        </>
      }
    >
      {/* A form, so Enter submits. Without one, the only way to commit a name is
          to leave the keyboard and press a button — which on a phone means
          dismissing the keyboard first to see it. */}
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (trimmed.length > 0 && !busy) onSubmit(trimmed);
        }}
      >
        <label htmlFor={inputId} className="sr-only">
          {label}
        </label>
        <input
          ref={inputRef}
          id={inputId}
          value={value}
          placeholder={placeholder}
          onChange={(event) => setValue(event.target.value)}
          disabled={busy}
          // `text-base` under a coarse pointer: mobile Safari zooms into any
          // focused input whose text is under 16px and does not zoom back out.
          className="w-full rounded-md border border-line-strong bg-sunken px-2 py-1.5 text-xs text-ink placeholder:text-ink-faint focus-ring pointer-coarse:text-base"
          // The name is the payload; the backend rejects a separator rather than
          // rewriting it, so nothing is filtered here — a rejected name comes
          // back as an error the user can see and fix.
          autoComplete="off"
          spellCheck={false}
        />
      </form>
    </DialogShell>
  );
}
