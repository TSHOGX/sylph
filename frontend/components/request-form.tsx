"use client";

/**
 * Rendering the input a request declared it wants.
 *
 * The counterpart to `lib/request-form.ts`: every rule about what an answer means
 * lives there, and this only draws. That split is what makes the rules assertable
 * without a DOM, and it is why this file has no validity logic of its own.
 *
 * # Why not `SelectRows`
 *
 * The app already has a choice-list renderer, and it is the wrong one here. Its rows
 * are `menuitem`/`menuitemradio`, which are valid only inside a `role="menu"`, while
 * this renders inside the approval panel's `role="dialog"` — putting menu items there
 * is invalid ARIA that leaves a screen reader with controls its container says cannot
 * exist. `SelectRows` also truncates its hint at `max-w-[45%]` in a 3xs mono face,
 * which is right for a keyboard shortcut and wrong for the sentence an agent wrote to
 * explain a choice. Native inputs in a fieldset are the correct construct for a form.
 */

import { useId } from "react";
import {
  fieldChoices,
  selectMode,
  type FormField,
  type FormState,
  type RequestForm as FormSpec,
} from "@/lib/request-form";
import { useT } from "@/lib/i18n";
import { Checkbox, FieldSet, Radio, TextArea, TextInput } from "@/components/ui/field";

interface Props {
  form: FormSpec;
  state: FormState;
  onChange: (state: FormState) => void;
  /** Set while an answer is in flight, so a form cannot be edited mid-send. */
  busy?: boolean;
  /**
   * Marks the first control, so the dialog can focus it. A data attribute rather
   * than a ref because the control is several components down and which element is
   * first depends on the declared field type.
   */
  firstFieldAttr?: string;
}

export function RequestFormFields({
  form,
  state,
  onChange,
  busy,
  firstFieldAttr,
}: Props) {
  const groupId = useId();

  function update(id: string, patch: Partial<FormState[string]>) {
    const entry = state[id];
    if (!entry) return;
    onChange({ ...state, [id]: { ...entry, ...patch } });
  }

  return (
    <div className="flex flex-col gap-4">
      {form.fields.map((field, index) => (
        <Field
          key={field.id}
          field={field}
          state={state}
          groupId={groupId}
          busy={busy}
          update={update}
          first={index === 0 ? firstFieldAttr : undefined}
        />
      ))}
    </div>
  );
}

function Field({
  field,
  state,
  groupId,
  busy,
  update,
  first,
}: {
  field: FormField;
  state: FormState;
  groupId: string;
  busy?: boolean;
  update: (id: string, patch: Partial<FormState[string]>) => void;
  first?: string;
}) {
  const t = useT();
  const entry = state[field.id];
  if (!entry) return null;

  if (field.type === "text") {
    const Control = field.multiline ? TextArea : TextInput;
    return (
      <label className="flex min-w-0 flex-col gap-1">
        <span className="text-sm font-medium text-ink">{field.label}</span>
        {field.description ? (
          <span className="text-xs text-ink-muted">{field.description}</span>
        ) : null}
        <Control
          value={entry.value}
          placeholder={field.placeholder}
          disabled={busy}
          spellCheck={!field.multiline ? false : undefined}
          data-first-field={first}
          onChange={(event) => update(field.id, { value: event.target.value })}
        />
      </label>
    );
  }

  const mode = selectMode(field);
  const choices = fieldChoices(field);
  // Namespaced by the field, so two questions on one panel are two radio groups
  // rather than one in which picking an answer to the second clears the first.
  const name = `${groupId}-${field.id}`;

  return (
    <FieldSet label={field.label} description={field.description}>
      {choices.map((choice, index) => {
        const shared = {
          name,
          label: choice.label ?? choice.value,
          description: choice.description,
          disabled: busy,
          "data-first-field": index === 0 ? first : undefined,
        };
        return mode === "many" ? (
          <Checkbox
            key={choice.value}
            {...shared}
            checked={entry.values.includes(choice.value)}
            onChange={(event) =>
              update(field.id, {
                values: event.target.checked
                  ? [...entry.values, choice.value]
                  : entry.values.filter((value) => value !== choice.value),
              })
            }
          />
        ) : (
          <Radio
            key={choice.value}
            {...shared}
            checked={!entry.other && entry.value === choice.value}
            onChange={() => update(field.id, { value: choice.value, other: false })}
          />
        );
      })}

      {/* The "Other" row, where the agent said an answer outside its list is
          allowed. What is typed here is the *answer*, not a note on one of the
          offered options - which is why it replaces the pick rather than riding
          alongside it.

          The text field is a sibling of the radio rather than a child of its label:
          nesting it would put it inside the label's click target, so typing in it
          would also re-toggle the radio, and it would inherit the radio's accessible
          name - leaving two controls a screen reader cannot tell apart. */}
      {field.allowOther && mode === "one" ? (
        <>
          <Radio
            name={name}
            label={field.otherLabel ?? t("form.other")}
            checked={entry.other}
            disabled={busy}
            onChange={() => update(field.id, { other: true })}
          />
          {entry.other ? (
            <TextInput
              value={entry.otherValue}
              placeholder={t("form.otherPlaceholder")}
              disabled={busy}
              aria-label={t("form.otherPlaceholder")}
              autoFocus
              className="-mt-0.5 ml-6 w-auto"
              onChange={(event) => update(field.id, { otherValue: event.target.value })}
            />
          ) : null}
        </>
      ) : null}

      {/* A note qualifies the pick rather than replacing it, so it sits below the
          whole group and is offered only where the agent accepts one. */}
      {field.notes ? (
        <label className="mt-1 flex min-w-0 flex-col gap-1">
          <span className="text-xs font-medium text-ink-muted">
            {field.notes.label ?? t("form.notes")}
          </span>
          <TextInput
            value={entry.notes}
            placeholder={field.notes.placeholder}
            disabled={busy}
            onChange={(event) => update(field.id, { notes: event.target.value })}
          />
        </label>
      ) : null}
    </FieldSet>
  );
}
