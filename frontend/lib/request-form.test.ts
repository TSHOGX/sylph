/**
 * The rules a request's declared form is answered by.
 *
 * These decide what the submit button offers and what shape reaches the agent, and
 * they have to match the server's own validation — a button that enables on an answer
 * the backend refuses tells the user nothing about what is wrong. Asserted here over
 * plain data, with nothing rendered, which is the point of keeping them in a pure
 * module.
 */

import { describe, expect, it } from "vitest";
import {
  formAnswer,
  initialFormState,
  isFormComplete,
  requestFormOf,
  selectMode,
  type RequestForm,
} from "@/lib/request-form";
import type { PendingRequest } from "@/lib/api-types";

function request(formJson?: string): PendingRequest {
  return {
    id: "r1",
    sessionId: "s1",
    turnId: "t1",
    kind: "choice",
    title: "Two questions",
    detailJson: "{}",
    optionsJson: JSON.stringify([{ id: "submit", label: "Submit" }]),
    formJson,
    status: "pending",
    createdAtMs: 1,
  };
}

const TWO_FIELDS: RequestForm = {
  fields: [
    {
      type: "select",
      id: "q:0",
      label: "Indentation?",
      mode: "one",
      choices: [{ value: "Tabs" }, { value: "Spaces" }],
      notes: { label: "Notes" },
    },
    {
      type: "select",
      id: "q:1",
      label: "Linters?",
      mode: "many",
      choices: [{ value: "ESLint" }, { value: "Clippy" }],
    },
  ],
};

describe("requestFormOf", () => {
  it("parses a declared form", () => {
    const form = requestFormOf(request(JSON.stringify(TWO_FIELDS)));
    expect(form?.fields).toHaveLength(2);
    expect(form?.fields[0].id).toBe("q:0");
  });

  it("reports no form when the request declared none", () => {
    // Which is what keeps every approval on the older detail-rendering path.
    expect(requestFormOf(request())).toBeNull();
  });

  it("degrades to no form rather than throwing on malformed JSON", () => {
    // A dialog that cannot render leaves the agent blocked with nothing to answer,
    // while a dialog with only its options is still answerable.
    expect(requestFormOf(request("{not json"))).toBeNull();
    expect(requestFormOf(request(JSON.stringify({ fields: "nope" })))).toBeNull();
  });

  it("drops a field it cannot render rather than the whole form", () => {
    const mixed = JSON.stringify({
      fields: [
        { type: "wormhole", id: "x" },
        { type: "text", id: "why", label: "Why?" },
      ],
    });
    expect(requestFormOf(request(mixed))?.fields).toHaveLength(1);
  });
});

describe("selectMode", () => {
  it("treats a field with no mode as single-select", () => {
    // The safe direction: reading one answer as a list would hand an agent an array
    // where it expected a string.
    expect(
      selectMode({ type: "select", id: "q", label: "", choices: [] }),
    ).toBe("one");
  });
});

describe("initialFormState", () => {
  it("seeds a text field from its prefill", () => {
    // Discarding it would lose the draft the agent asked the user to revise.
    const state = initialFormState({
      fields: [{ type: "text", id: "why", label: "Why?", prefill: "because" }],
    });
    expect(state.why.value).toBe("because");
  });

  it("leaves a select unanswered", () => {
    // Pre-selecting an option the user never read is an answer they did not give,
    // which is the one thing this channel exists to prevent.
    const state = initialFormState(TWO_FIELDS);
    expect(state["q:0"].value).toBe("");
    expect(state["q:1"].values).toEqual([]);
  });
});

describe("isFormComplete", () => {
  it("allows submitting a form that required nothing", () => {
    expect(isFormComplete(TWO_FIELDS, initialFormState(TWO_FIELDS))).toBe(true);
  });

  it("withholds submit until a required field is answered", () => {
    const form: RequestForm = {
      fields: [{ type: "text", id: "why", label: "Why?", required: true }],
    };
    const state = initialFormState(form);
    expect(isFormComplete(form, state)).toBe(false);

    // Whitespace is unanswered, matching the server: sending it would report an
    // answer the user never gave.
    expect(isFormComplete(form, { why: { ...state.why, value: "   " } })).toBe(false);
    expect(isFormComplete(form, { why: { ...state.why, value: "because" } })).toBe(true);
  });

  it("counts a required select as answered only once something is picked", () => {
    const form: RequestForm = {
      fields: [
        {
          type: "select",
          id: "q",
          label: "Pick",
          mode: "many",
          choices: [{ value: "a" }],
          required: true,
        },
      ],
    };
    const state = initialFormState(form);
    expect(isFormComplete(form, state)).toBe(false);
    expect(isFormComplete(form, { q: { ...state.q, values: ["a"] } })).toBe(true);
  });
});

describe("formAnswer", () => {
  it("sends a pick, a list, and a note in the resolve envelope", () => {
    const state = initialFormState(TWO_FIELDS);
    const answer = formAnswer(TWO_FIELDS, {
      "q:0": { ...state["q:0"], value: "Tabs", notes: "only for generated files" },
      "q:1": { ...state["q:1"], values: ["ESLint", "Clippy"] },
    });
    expect(answer).toEqual({
      values: { "q:0": "Tabs", "q:1": ["ESLint", "Clippy"] },
      notes: { "q:0": "only for generated files" },
    });
  });

  it("sends free text as the answer, not as a note", () => {
    // What the terminal's "Other" row does. A custom answer replaces the pick; a
    // note qualifies one, and conflating them loses which was meant.
    const form: RequestForm = {
      fields: [
        {
          type: "select",
          id: "q",
          label: "Pick",
          choices: [{ value: "Tabs" }],
          allowOther: true,
        },
      ],
    };
    const state = initialFormState(form);
    const answer = formAnswer(form, {
      q: { ...state.q, value: "Tabs", other: true, otherValue: "Tabs in Makefiles" },
    });
    expect(answer.values).toEqual({ q: "Tabs in Makefiles" });
    expect(answer.notes).toBeUndefined();
  });

  it("omits a field left blank rather than sending an empty answer", () => {
    // So the agent can tell "left blank" from "answered with nothing" — a
    // distinction the CLI makes when it builds the tool result the model reads.
    const answer = formAnswer(TWO_FIELDS, initialFormState(TWO_FIELDS));
    expect(answer.values).toEqual({});
  });

  it("drops a note on a field that never offered one", () => {
    // The server refuses it, so this keeps the refusal unreachable rather than
    // merely handled.
    const state = initialFormState(TWO_FIELDS);
    const answer = formAnswer(TWO_FIELDS, {
      ...state,
      "q:1": { ...state["q:1"], values: ["ESLint"], notes: "sneaky" },
    });
    expect(answer.notes).toBeUndefined();
  });

  it("keeps the typed other text when the user switches back to an option", () => {
    // Held separately from the pick so reconsidering twice does not destroy either.
    const form: RequestForm = {
      fields: [
        {
          type: "select",
          id: "q",
          label: "Pick",
          choices: [{ value: "Tabs" }],
          allowOther: true,
        },
      ],
    };
    const state = initialFormState(form);
    const answer = formAnswer(form, {
      q: { ...state.q, value: "Tabs", other: false, otherValue: "still here" },
    });
    expect(answer.values).toEqual({ q: "Tabs" });
  });
});
