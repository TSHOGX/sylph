/**
 * The answer model behind a request's declared form.
 *
 * # Why this is a module of its own
 *
 * A form's rules — what a field accepts, what counts as answered, what the resolve
 * envelope looks like — are decided by the agent that raised the request and checked
 * by the server. The client has to agree with both, or the submit button enables on
 * an answer the backend will refuse and the user is told nothing useful.
 *
 * Keeping the rules here, pure and with no DOM, is what makes that agreement
 * testable: `request-form.test.ts` asserts them without rendering anything, the same
 * split `select-list.ts` makes for the same reason. The renderer only draws.
 *
 * # Declared, never inferred
 *
 * Before forms existed, the dialog guessed what to collect by looking for field
 * names it recognized in `detail` — `command`, `file_path`, `url`. A question's
 * detail carries none of those, so an agent's question rendered as bare buttons with
 * nowhere to type, and an `elicitation` — whose entire purpose is free text —
 * rendered as a read-only dump of its own placeholder. Guessing cannot be repaired,
 * only replaced: only the agent knows what it is asking for.
 *
 * The server is still authoritative. Everything here is what lets the UI *say* an
 * answer is incomplete before sending it; it is not what makes it safe.
 */

import type { PendingRequest } from "./api";

/** One choice within a select field. */
export interface FormChoice {
  value: string;
  /** Display text, when it differs from the value. */
  label?: string;
  /**
   * What choosing this means.
   *
   * Load-bearing on a question the agent authored rather than decorative: "Tabs"
   * and "Spaces" say far less than the sentences beside them.
   */
  description?: string;
}

/** A free-text annotation attached to an answer rather than replacing it. */
export interface NotesSpec {
  label?: string;
  placeholder?: string;
}

export interface SelectField {
  type: "select";
  id: string;
  label: string;
  description?: string;
  /** `one` renders radios, `many` checkboxes. Absent is `one`. */
  mode?: "one" | "many";
  choices: FormChoice[];
  /**
   * Whether an answer outside `choices` is allowed.
   *
   * Claude's question dialog always offers an "Other" row, and what a user types
   * there is the *answer* — not a note on one of the offered options.
   */
  allowOther?: boolean;
  otherLabel?: string;
  notes?: NotesSpec;
  required?: boolean;
}

export interface TextField {
  type: "text";
  id: string;
  label: string;
  description?: string;
  multiline?: boolean;
  /** Text to start from, which the user edits. */
  prefill?: string;
  placeholder?: string;
  required?: boolean;
}

export type FormField = SelectField | TextField;

export interface RequestForm {
  fields: FormField[];
}

/**
 * What the user has entered, keyed by field id.
 *
 * `other` is held separately from `value` so switching to the "Other" row and back
 * does not destroy either one — a user who types a custom answer, reconsiders, picks
 * an offered option, then reconsiders again should still find their text.
 */
export interface FieldState {
  /** The picked choice, or the typed text for a text field. */
  value: string;
  /** Picked values for a `many` select. */
  values: string[];
  /** Whether the "Other" row is the active answer for this field. */
  other: boolean;
  /** Free text for the "Other" row. */
  otherValue: string;
  /** The note attached to this answer. */
  notes: string;
}

export type FormState = Record<string, FieldState>;

/** The resolve envelope: `{values, notes}` under `input`. */
export interface FormAnswer {
  values: Record<string, string | string[]>;
  notes?: Record<string, string>;
}

/**
 * Parse a request's declared form. `null` when it declared none, which means the
 * older `detail`-rendering path — right for an approval, whose subject is one
 * proposed tool input.
 *
 * Never throws. A malformed form renders as no form rather than breaking the dialog
 * that has to display it, matching `requestDetail`/`requestOptions`: the request is
 * still answerable through its options, and refusing to render leaves the agent
 * blocked over a display concern.
 */
export function requestFormOf(request: PendingRequest): RequestForm | null {
  if (!request.formJson) return null;
  try {
    const parsed = JSON.parse(request.formJson);
    if (!parsed || typeof parsed !== "object") return null;
    const fields = (parsed as RequestForm).fields;
    if (!Array.isArray(fields)) return null;
    const usable = fields.filter(
      (field) =>
        field &&
        typeof field.id === "string" &&
        (field.type === "select" || field.type === "text"),
    );
    return usable.length > 0 ? { fields: usable } : null;
  } catch {
    return null;
  }
}

export function selectMode(field: SelectField): "one" | "many" {
  return field.mode === "many" ? "many" : "one";
}

/** The choices to render, plus whether an "Other" row belongs after them. */
export function fieldChoices(field: SelectField): FormChoice[] {
  return Array.isArray(field.choices) ? field.choices : [];
}

/**
 * The starting state for a form.
 *
 * A text field seeds from `prefill`, because discarding it would lose the draft the
 * agent asked the user to revise. A select seeds empty: pre-selecting an option the
 * user never read is an answer they did not give, which is the one thing this whole
 * channel exists to prevent.
 */
export function initialFormState(form: RequestForm): FormState {
  const state: FormState = {};
  for (const field of form.fields) {
    state[field.id] = {
      value: field.type === "text" ? (field.prefill ?? "") : "",
      values: [],
      other: false,
      otherValue: "",
      notes: "",
    };
  }
  return state;
}

/** The answer a field currently holds, in the shape the wire expects. */
function fieldValue(field: FormField, entry: FieldState): string | string[] {
  if (field.type === "text") return entry.value;
  if (selectMode(field) === "many") return entry.values;
  return entry.other ? entry.otherValue : entry.value;
}

/**
 * Whether a field has an answer.
 *
 * Blank is unanswered rather than answered-with-nothing, matching the server: an
 * agent that marked a field required is blocked until it has something, and sending
 * `""` would report an answer it never received.
 */
function isAnswered(value: string | string[]): boolean {
  return Array.isArray(value) ? value.length > 0 : value.trim().length > 0;
}

/**
 * Whether this state may be submitted.
 *
 * Deliberately the same predicate the backend applies, so the button and the server
 * agree — a submit that enables on an answer the server refuses tells the user
 * nothing about what is wrong. The server stays authoritative; this only decides
 * whether to *offer* the action.
 *
 * A form with no required fields is submittable while empty, because an agent that
 * required nothing is asking a question the user may legitimately answer with
 * nothing but a note.
 */
export function isFormComplete(form: RequestForm, state: FormState): boolean {
  return form.fields.every((field) => {
    if (!field.required) return true;
    const entry = state[field.id];
    return entry ? isAnswered(fieldValue(field, entry)) : false;
  });
}

/**
 * The resolve envelope for this state.
 *
 * Only answered fields are included. An unanswered optional field is omitted rather
 * than sent as `""`, so the agent can tell "left blank" from "answered with nothing"
 * — a distinction `claude` makes when it builds the tool result the model reads.
 */
export function formAnswer(form: RequestForm, state: FormState): FormAnswer {
  const values: Record<string, string | string[]> = {};
  const notes: Record<string, string> = {};

  for (const field of form.fields) {
    const entry = state[field.id];
    if (!entry) continue;

    const value = fieldValue(field, entry);
    if (isAnswered(value)) values[field.id] = value;

    // A note rides along only where the agent offered one, or the server refuses it
    // — the same rule, checked here so the refusal is unreachable rather than
    // merely handled.
    if (field.type === "select" && field.notes && entry.notes.trim()) {
      notes[field.id] = entry.notes.trim();
    }
  }

  return Object.keys(notes).length > 0 ? { values, notes } : { values };
}
