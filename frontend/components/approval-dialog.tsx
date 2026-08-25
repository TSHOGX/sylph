"use client";

/**
 * Answering what an agent is blocked on.
 *
 * This is the surface the v2 protocol exists to make possible. An agent that
 * needs permission stops and waits, so until this is answered the session does
 * nothing — which makes it the one dialog in the app that must never be dismissed
 * into oblivion. Escape closes the panel but does *not* resolve the request; it
 * stays listed, and the composer shows that the agent is still waiting.
 *
 * # Two shapes, and why the request decides which
 *
 * An **approval** is about one proposed tool input: the panel renders that input,
 * lets it be edited, and offers allow/deny verbs. A **question** is not that — the
 * agent authored the choices, there may be several questions, and the answer can
 * legitimately be something the agent did not offer. So a request may *declare* the
 * input it wants (`form`), and when it does, that is rendered verbatim.
 *
 * Declared, never inferred. This file used to guess by looking for field names it
 * recognized in `detail` — and a question's detail contains none of them, so a
 * `choice` rendered as bare buttons with nowhere to type and an `elicitation`, whose
 * entire purpose is free text, rendered as a read-only dump of its own placeholder.
 * Only the agent knows what it is asking for. See `docs/session-protocol.md`
 * § Request forms.
 *
 * # Why the proposed input is editable
 *
 * A terminal lets you say "yes, but not quite that". Reducing an approval to
 * yes/no throws that away, and it is the difference between rejecting a whole
 * turn and adjusting one flag on one command. The edited value is what the
 * backend forwards, so what the agent runs is exactly what the user sees here.
 *
 * # Why deny is the default
 *
 * The dialog focuses its deny affordance, matching `AlertDialog`'s reasoning: a
 * stray Enter on a dialog that just appeared must not grant a permission. The
 * agent's own `defaultOption` is honoured when it names one, and it is deny in
 * practice for anything destructive. A form is the one exception: there the first
 * control is focused instead, because an unanswered form cannot be submitted by
 * Enter anyway and starting on the deny row would make answering take an extra key.
 */

import { useEffect, useMemo, useState } from "react";
import {
  requestDetail,
  requestOptions,
  type PendingRequest,
  type RequestOption,
} from "@/lib/api";
import {
  formAnswer,
  initialFormState,
  isFormComplete,
  requestFormOf,
  type FormState,
} from "@/lib/request-form";
import { RequestFormFields } from "@/components/request-form";
import { useDialog } from "@/lib/use-dialog";
import { useT } from "@/lib/i18n";
import { Button } from "@/components/ui/button";

interface Props {
  request: PendingRequest;
  /** Answer it. `input` is the edited proposal, or undefined for as-proposed. */
  onResolve: (option: string, input?: unknown) => void;
  /** Hide the panel without answering. The request stays open. */
  onDismiss: () => void;
  /** Set while an answer is in flight, so it cannot be sent twice. */
  busy?: boolean;
}

/** Which detail field carries the thing a user would want to edit.
 *
 * Ordered by how often it is the whole point of the approval: a shell command is
 * both the most common and the most consequential, and a path or URL is the next.
 * Anything else renders read-only as JSON, because guessing at an arbitrary
 * schema's editable field would produce a form that silently drops the rest. */
const EDITABLE_FIELDS = ["command", "file_path", "path", "url", "pattern"] as const;

function editableField(detail: Record<string, unknown>): string | null {
  return (
    EDITABLE_FIELDS.find((field) => typeof detail[field] === "string") ?? null
  );
}

/** Emphasis for an option, so allow and deny do not look identical.
 *
 * A persistent grant is styled as the weightiest thing on the panel, because
 * "always allow" is a policy change rather than one decision and the two should
 * not be one careless click apart. */
function optionVariant(option: RequestOption, risky: boolean) {
  if (option.id.startsWith("deny") || option.id === "cancel") return "secondary" as const;
  if (option.persistent) return risky ? ("danger" as const) : ("secondary" as const);
  return risky ? ("danger" as const) : ("primary" as const);
}

export function ApprovalDialog({ request, onResolve, onDismiss, busy }: Props) {
  const t = useT();
  const detail = useMemo(() => requestDetail(request), [request]);
  const options = useMemo(() => requestOptions(request), [request]);
  const form = useMemo(() => requestFormOf(request), [request]);
  const field = useMemo(
    // A declared form replaces the guess entirely. Sniffing `detail` alongside one
    // would render a second, unrelated editor beside the fields the agent asked for.
    () => (form ? null : editableField(detail)),
    [form, detail],
  );

  const proposed = field ? String(detail[field] ?? "") : "";
  const [edited, setEdited] = useState(proposed);
  const [formState, setFormState] = useState<FormState>(() =>
    form ? initialFormState(form) : {},
  );
  const [editedFor, setEditedFor] = useState(request.id);

  // Reset when a different request arrives in the same mounted dialog. Without
  // this, answering one approval and being handed another would carry the first
  // one's edited command into the second - approving something never proposed, and
  // for a form, submitting one question's answers against another's fields.
  //
  // Done during render rather than in an effect, which is what React prescribes
  // for state derived from a changing input. An effect would paint one frame
  // showing the previous request's command under the new request's title, and that
  // frame is one a fast click would approve.
  if (editedFor !== request.id) {
    setEditedFor(request.id);
    setEdited(proposed);
    setFormState(form ? initialFormState(form) : {});
  }

  const risky = request.risk === "destructive";
  // A question the agent asked, rather than permission it needs. Either it declared
  // a form, or it is a `choice` whose options are themselves the answers - the older
  // shape, still rendered so an adapter that has not been updated keeps working.
  const asking = Boolean(form) || request.kind === "choice";
  const changed = field !== null && edited !== proposed;
  const complete = form ? isFormComplete(form, formState) : true;

  const { ref, onKeyDown } = useDialog<HTMLDivElement>({
    onClose: onDismiss,
    autoFocus: false,
  });

  // Focus the refusing option rather than the first control, so an Enter that
  // arrives together with the dialog cannot grant a permission. Queried by data
  // attribute because `Button` takes no ref, and because the option's *label* is
  // localized while its id is not.
  //
  // A form focuses its first field instead: there is nothing to grant by pressing
  // Enter on an unanswered form, and landing on the deny row would cost a keystroke
  // on the common path.
  //
  // Deferred by a microtask rather than run synchronously. React runs child effects
  // before parent ones, and `useDialog` captures the opener in its own mount effect,
  // so a focus call here would be captured *as* the opener - and closing the panel
  // would then "restore" focus to an unmounting node and drop it on `<body>`
  // (WCAG 2.4.3).
  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (cancelled) return;
      const container = ref.current;
      if (!container) return;
      const target =
        container.querySelector<HTMLElement>("[data-first-field]") ??
        container.querySelector<HTMLElement>('[data-deny="true"]');
      target?.focus();
    });
    return () => {
      cancelled = true;
    };
  }, [request.id, ref]);

  function resolve(option: string) {
    if (busy) return;
    if (form) {
      // Declining carries no answers: a required field must not become a trap the
      // user cannot leave by saying "I would rather not say".
      const declining = option.startsWith("deny") || option === "cancel";
      onResolve(option, declining ? undefined : formAnswer(form, formState));
      return;
    }
    // Only send an input when it differs. Echoing an unchanged proposal back
    // would be harmless but makes the audit record ambiguous about whether the
    // user edited anything.
    onResolve(option, changed ? { ...detail, [field as string]: edited } : undefined);
  }

  return (
    <div
      ref={ref}
      onKeyDown={onKeyDown}
      role="dialog"
      aria-modal="true"
      aria-labelledby={`approval-title-${request.id}`}
      className="flex flex-col gap-3 rounded-lg border border-warning-line bg-raised p-4 shadow-lg"
      data-testid="approval-dialog"
    >
      <div className="flex items-start gap-2">
        <span
          aria-hidden
          className={`mt-0.5 size-2 shrink-0 rounded-full ${
            risky ? "bg-danger-solid" : "bg-warning-solid"
          }`}
        />
        <div className="min-w-0 flex-1">
          <h2
            id={`approval-title-${request.id}`}
            className="text-sm font-medium text-ink"
          >
            {request.title || t("approval.fallbackTitle")}
          </h2>
          <p className="mt-0.5 text-xs text-ink-muted">
            {request.tool && !asking
              ? t("approval.agentWantsTool", { tool: request.tool })
              : asking
                ? t("approval.question")
                : t("approval.agentWants")}
          </p>
        </div>
      </div>

      {form ? (
        <RequestFormFields
          form={form}
          state={formState}
          onChange={setFormState}
          busy={busy}
          firstFieldAttr="true"
        />
      ) : asking ? (
        <div className="flex flex-col gap-1.5">
          {options.map((option) => {
            const declining = option.id.startsWith("deny");
            const description = option.description;
            return (
              <button
                key={option.id}
                type="button"
                data-deny={declining ? "true" : undefined}
                disabled={busy}
                onClick={() => resolve(option.id)}
                className={`focus-ring tap-target flex flex-col items-start gap-0.5 rounded-lg border px-3 py-2 text-left transition-colors disabled:opacity-50 ${
                  declining
                    ? "border-line text-ink-muted hover:bg-hover"
                    : "border-line-strong text-ink hover:bg-active"
                }`}
              >
                <span className="text-sm font-medium">{option.label}</span>
                {description ? (
                  <span className="text-xs text-ink-muted">{description}</span>
                ) : null}
              </button>
            );
          })}
        </div>
      ) : field ? (
        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium text-ink-muted">
            {t("approval.proposed")}
          </span>
          <textarea
            value={edited}
            onChange={(event) => setEdited(event.target.value)}
            spellCheck={false}
            rows={Math.min(6, edited.split("\n").length + 1)}
            // 16px floor under coarse pointers, or mobile Safari zooms on focus
            // and does not zoom back out.
            className="resize-y rounded border border-line bg-sunken px-2 py-1.5 font-mono text-xs text-ink outline-none focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus [@media(pointer:coarse)]:text-base"
            aria-label={t("approval.proposed")}
          />
          {changed ? (
            <span className="text-xs text-warning">{t("approval.edited")}</span>
          ) : null}
        </label>
      ) : Object.keys(detail).length > 0 ? (
        <pre className="max-h-40 overflow-auto rounded border border-line bg-sunken px-2 py-1.5 font-mono text-xs text-ink-muted">
          {JSON.stringify(detail, null, 2)}
        </pre>
      ) : null}

      {/* Verbs. A form renders them too: with the fields carrying the answer, an
          option is what to *do* with it - send it, or decline to give one - which is
          what makes "pick, then add a note, then submit" expressible at all. The
          older `choice` shape, where the options are themselves the answers, renders
          them as the rows above instead. */}
      {(form || !asking) && (
      <div className="flex flex-col gap-1.5">
        <div className="flex flex-wrap gap-2">
          {options.map((option) => {
            const denying = option.id.startsWith("deny") || option.id === "cancel";
            return (
              <Button
                key={option.id}
                data-deny={denying ? "true" : undefined}
                variant={optionVariant(option, risky)}
                size="sm"
                // An incomplete form blocks only the submitting verbs. Declining
                // must stay available, or a required field is a trap.
                disabled={busy || (Boolean(form) && !denying && !complete)}
                onClick={() => resolve(option.id)}
              >
                {option.label}
              </Button>
            );
          })}
        </div>
        {/* Said rather than left to be inferred from a greyed button: a disabled
            control with no explanation is the shape that reads as broken. */}
        {form && !complete ? (
          <p className="text-2xs text-ink-muted">{t("approval.incomplete")}</p>
        ) : null}
      </div>
      )}
    </div>
  );
}
