"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentCommand } from "@/lib/api";

export interface CommandInvocation {
  command: string;
  args: Record<string, string>;
}

interface Props {
  /** Commands the agent declared in its `--describe` output. */
  commands: Record<string, AgentCommand>;
  onSubmit: (invocation: CommandInvocation) => void;
  onClose: () => void;
  /** Set while a command is being sent, to keep it from being sent twice. */
  busy?: boolean;
}

/**
 * Run a named agent command, such as a model or effort change.
 *
 * The command list and every argument come from the agent's own `--describe`
 * output rather than a hardcoded menu, so an agent that declares no commands
 * simply has no dialog. Nothing here rewrites the prompt or resets the session;
 * the backend passes the invocation to the runner as-is, which is what keeps
 * native sessions usable through this dialog too.
 */
export function CommandDialog({ commands, onSubmit, onClose, busy }: Props) {
  const names = useMemo(() => Object.keys(commands).sort(), [commands]);
  const [selected, setSelected] = useState(names[0] ?? "");
  const [args, setArgs] = useState<Record<string, string>>({});
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstFieldRef = useRef<HTMLSelectElement>(null);

  const spec = commands[selected];
  const argSpecs = useMemo(() => Object.entries(spec?.args ?? {}), [spec]);

  // Move focus into the dialog on open, so keyboard users are not left behind
  // on the composer while a modal covers it. On close, put focus back where it
  // came from: without this, dismissing the dialog drops a keyboard user at the
  // top of the document and makes them tab back to the composer (WCAG 2.4.3).
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    firstFieldRef.current?.focus();
    return () => opener?.focus?.();
  }, []);

  // Escape closes, and Tab is trapped inside: a modal that leaks focus to the
  // page behind it is unusable without a mouse.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
        "button, select, input, textarea, [href]",
      );
      if (!focusable || focusable.length === 0) return;

      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }

    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const missingRequired = argSpecs.some(
    ([name, arg]) => arg.required && !(args[name] ?? "").trim(),
  );

  function submit() {
    if (!selected || missingRequired || busy) return;
    const filled = Object.fromEntries(
      Object.entries(args).filter(([, value]) => value.trim() !== ""),
    );
    onSubmit({ command: selected, args: filled });
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-ink/35 p-4">
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="command-dialog-title"
        className="w-full max-w-md rounded-lg bg-raised p-5 shadow-xl"
      >
        <h2 id="command-dialog-title" className="text-lg font-semibold">
          Run a command
        </h2>

        <div className="mt-4">
          <label htmlFor="command-name" className="block text-xs font-medium text-ink-muted">
            Command
          </label>
          <select
            id="command-name"
            ref={firstFieldRef}
            value={selected}
            onChange={(e) => {
              setSelected(e.target.value);
              // Arguments are per-command, so carrying them across would send a
              // value the newly selected command never declared.
              setArgs({});
            }}
            className="mt-1 w-full rounded border px-2 py-1.5 text-sm focus-ring"
          >
            {names.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
          {spec?.description && (
            <p className="mt-1 text-xs text-ink-muted">{spec.description}</p>
          )}
        </div>

        {argSpecs.map(([name, arg]) => {
          const fieldId = `command-arg-${name}`;
          return (
            <div key={name} className="mt-3">
              <label htmlFor={fieldId} className="block text-xs font-medium text-ink-muted">
                {name}
                {arg.required && <span className="text-danger"> (required)</span>}
              </label>
              <input
                id={fieldId}
                type="text"
                value={args[name] ?? ""}
                required={arg.required}
                aria-describedby={arg.description ? `${fieldId}-hint` : undefined}
                onChange={(e) => setArgs({ ...args, [name]: e.target.value })}
                className="mt-1 w-full rounded border px-2 py-1.5 text-sm focus-ring"
              />
              {arg.description && (
                <p id={`${fieldId}-hint`} className="mt-1 text-xs text-ink-muted">
                  {arg.description}
                </p>
              )}
            </div>
          );
        })}

        <div className="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border px-4 py-2 text-sm font-medium hover:bg-sunken focus-ring"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={missingRequired || busy || !selected}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-accent-ink hover:bg-accent-hover focus-ring disabled:opacity-50"
          >
            {busy ? "Running..." : "Run"}
          </button>
        </div>
      </div>
    </div>
  );
}
