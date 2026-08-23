/**
 * Buttons.
 *
 * # Why these exist
 *
 * Before this module the app contained roughly forty hand-written button class
 * strings and 92 copies of `focus:outline-none focus-visible:ring-2
 * focus-visible:ring-gray-900`. They were not identical — padding drifted between
 * `px-2 py-1` and `px-2.5 py-1`, disabled state was sometimes `opacity-50` and
 * sometimes nothing, and hover was `hover:bg-gray-100` in the top bar but
 * `hover:bg-gray-200/60` in the sidebar for the same kind of control. That drift
 * is most of what reads as unfinished, and it is not fixable by being careful: it
 * is fixable by there being one definition.
 *
 * # Variants are semantic, not visual
 *
 * `primary` is the one action a surface is *for*; `danger` destroys or interrupts;
 * `secondary` is a bordered alternative; `ghost` is chrome. A caller picks by
 * meaning, so a later change to what "chrome" looks like lands everywhere at once.
 *
 * `tap-target` on every variant: on a coarse pointer these grow to 44px, and stay
 * dense on a mouse. A control that is finger-sized on a desktop wastes the pane;
 * one that is mouse-sized on a phone cannot be hit.
 */

import type { ButtonHTMLAttributes, ReactNode } from "react";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger" | "chip";
export type ButtonSize = "sm" | "md";

const VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-accent text-accent-ink hover:bg-accent-hover disabled:opacity-50",
  secondary:
    "border border-line-strong text-ink hover:bg-hover disabled:opacity-50",
  ghost: "text-ink-muted hover:bg-hover hover:text-ink disabled:opacity-50",
  danger:
    "border border-danger-line text-danger hover:bg-danger-bg disabled:opacity-50",
  /**
   * A compact bordered pill for a *current value* that opens a picker — the
   * composer's model and thinking controls. Distinct from `secondary` because it
   * must be able to truncate: a long model name has to shorten rather than push
   * the row past the viewport, which is what the previous `<select>` did.
   */
  chip: "border border-line text-ink-soft hover:bg-hover hover:text-ink disabled:opacity-50",
};

const SIZES: Record<ButtonSize, string> = {
  sm: "gap-1.5 rounded-md px-2 py-1 text-2xs",
  md: "gap-1.5 rounded-lg px-3 py-1.5 text-xs",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Leading icon. Decorative — the label carries the accessible name. */
  icon?: ReactNode;
  children?: ReactNode;
}

export function Button({
  variant = "secondary",
  size = "sm",
  icon,
  children,
  className = "",
  type = "button",
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={`focus-ring tap-target inline-flex items-center justify-center font-medium transition-colors disabled:cursor-not-allowed ${SIZES[size]} ${VARIANTS[variant]} ${className}`}
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}

/**
 * A square button whose only content is an icon.
 *
 * `label` is required and is not optional-by-convention: an icon-only control with
 * no accessible name is invisible to a screen reader, and this is the single most
 * repeated shape in the app. Making it a required prop means the failure is a type
 * error rather than an audit finding.
 *
 * `pressed` is passed through to `aria-pressed` only when defined, so a plain
 * action button is not announced as an un-pressed toggle.
 */
export function IconButton({
  label,
  hint,
  pressed,
  variant = "ghost",
  size = 16,
  className = "",
  children,
  type = "button",
  ref,
  ...rest
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> & {
  label: string;
  /** Keyboard hint appended to the tooltip, e.g. "⌘K". */
  hint?: string;
  pressed?: boolean;
  variant?: ButtonVariant;
  /** Icon edge length; the hit area is set by padding and `tap-target`. */
  size?: number;
  children: ReactNode;
  /**
   * A plain prop, not `forwardRef`. React 19 passes `ref` through to function
   * components directly, and `forwardRef` is deprecated. Needed because the
   * Artifacts pane holds a live reference to the viewer's expand button in order to
   * restore focus to it after the full-screen dialog closes — `Overlay` cannot,
   * since that button is unmounted while the dialog is up.
   */
  ref?: React.Ref<HTMLButtonElement>;
}) {
  return (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      aria-pressed={pressed}
      title={hint ? `${label} (${hint})` : label}
      className={`focus-ring tap-target inline-flex items-center justify-center rounded-lg transition-colors disabled:cursor-not-allowed ${
        size >= 20 ? "p-2" : "p-1.5"
      } ${VARIANTS[variant]} ${className}`}
      {...rest}
    >
      {children}
    </button>
  );
}

/**
 * A keyboard cap.
 *
 * Lived in `app-shell.tsx`, which meant every surface teaching a shortcut had to
 * import the shell to render one. `font-sans` explicitly: a monospace cap at 10px
 * is wider than the key it names and wraps in the palette's tight rows.
 */
export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border border-line bg-raised px-1 py-px font-sans text-3xs text-ink-muted">
      {children}
    </kbd>
  );
}
