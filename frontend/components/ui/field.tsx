/**
 * Form controls: a text field, a radio, and a checkbox.
 *
 * # Why these exist
 *
 * The app had no such primitives. Every place that needed a text input hand-rolled
 * one — `PromptDialog` embeds an `<input>`, `SelectFilter` another, and the approval
 * dialog a raw `<textarea>` with an inline class string — and there was no radio or
 * checkbox at all, because until a request could declare a form nothing needed one.
 *
 * Adding them here rather than inside the one consumer is the lesson `button.tsx`
 * records: forty hand-written class strings drifted in padding, disabled state, and
 * hover, and that drift is most of what reads as unfinished. It is not fixable by
 * being careful, only by there being one definition.
 *
 * # Two rules these encode that are easy to get wrong
 *
 * **16px minimum text under a coarse pointer.** Mobile Safari zooms when a focused
 * input's text is smaller and does not zoom back out, which presents as "the whole
 * screen moved" and reads as a layout bug. The floor applies only under
 * `pointer: coarse`, because dense chrome legitimately uses 11–12px and inflating
 * every input to fix a mobile-only problem is the wrong trade.
 *
 * **A native input, styled — not a div with ARIA.** A `<input type="radio">` in a
 * named group gets arrow-key navigation, the roving tab stop, and grouping semantics
 * from the platform. Rebuilding those on divs is how a keyboard user ends up with a
 * choice list they cannot move through, and `accent-color` is enough to make the
 * native control match the theme.
 */

import type { InputHTMLAttributes, ReactNode, TextareaHTMLAttributes } from "react";

/**
 * The shared field surface. Kept in one string so an input and a textarea cannot
 * drift, which is exactly what happened between the three hand-rolled copies.
 */
const FIELD =
  "w-full rounded border border-line bg-sunken px-2 py-1.5 text-xs text-ink placeholder:text-ink-faint " +
  "focus-ring disabled:opacity-50 [@media(pointer:coarse)]:text-base";

export interface TextInputProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "type"> {
  /** Monospace, for a path or a command. */
  mono?: boolean;
}

export function TextInput({ mono, className = "", ...rest }: TextInputProps) {
  return (
    <input
      type="text"
      className={`${FIELD} ${mono ? "font-mono" : ""} ${className}`}
      {...rest}
    />
  );
}

export interface TextAreaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  mono?: boolean;
}

export function TextArea({ mono, className = "", rows = 3, ...rest }: TextAreaProps) {
  return (
    <textarea
      rows={rows}
      className={`${FIELD} resize-y ${mono ? "font-mono" : ""} ${className}`}
      {...rest}
    />
  );
}

interface ChoiceProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "type"> {
  label: ReactNode;
  /** What choosing this means. Rendered under the label, wrapping freely. */
  description?: ReactNode;
  /** Trailing content on the label's own row — an "Other" text field, say. */
  children?: ReactNode;
}

/**
 * One row of a radio or checkbox group.
 *
 * The whole row is the label, so the description and the generous padding are part
 * of the hit target rather than dead space beside it — which matters most on a phone,
 * where the control itself is 16px in a 44px row.
 *
 * `tap-row` rather than `tap-target`: only height applies to a full-width row, and
 * `tap-target`'s `min-width` would fight the layout.
 */
function Choice({
  type,
  label,
  description,
  children,
  className = "",
  ...rest
}: ChoiceProps & { type: "radio" | "checkbox" }) {
  return (
    <label
      className={`tap-row flex cursor-pointer items-start gap-2 rounded-lg border border-line px-3 py-2 transition-colors hover:bg-hover has-checked:border-line-strong has-checked:bg-active has-disabled:cursor-not-allowed has-disabled:opacity-50 ${className}`}
    >
      <input
        type={type}
        // `accent-color` colours the native control from the theme, which is what
        // lets this stay a real input rather than a div wearing ARIA.
        className="focus-ring mt-0.5 size-4 shrink-0 accent-[var(--color-accent)]"
        {...rest}
      />
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="text-sm text-ink">{label}</span>
        {description ? (
          <span className="text-xs text-ink-muted">{description}</span>
        ) : null}
        {children}
      </span>
    </label>
  );
}

export function Radio(props: ChoiceProps) {
  return <Choice type="radio" {...props} />;
}

export function Checkbox(props: ChoiceProps) {
  return <Choice type="checkbox" {...props} />;
}

/**
 * A labelled group of controls.
 *
 * A `<fieldset>`/`<legend>`, which is the only construct that associates one label
 * with several controls without inventing `aria-labelledby` wiring per call site. The
 * legend is styled rather than visually hidden: a question the agent asked *is* the
 * heading of its own choices.
 */
export function FieldSet({
  label,
  description,
  children,
}: {
  label: ReactNode;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <fieldset className="min-w-0">
      <legend className="mb-1.5 text-sm font-medium text-ink">{label}</legend>
      {description ? (
        <p className="mb-1.5 text-xs text-ink-muted">{description}</p>
      ) : null}
      <div className="flex flex-col gap-1.5">{children}</div>
    </fieldset>
  );
}
