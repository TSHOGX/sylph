/**
 * Parsing what an agent proposed, and what it offered.
 *
 * These two functions stand between an agent's JSON and a dialog that must always
 * render. A malformed payload here would leave a blocked agent with no answerable
 * prompt on screen, so both degrade rather than throw.
 */

import { describe, expect, it } from "vitest";
import { requestDetail, requestOptions, type PendingRequest } from "@/lib/api";

function request(overrides: Partial<PendingRequest> = {}): PendingRequest {
  return {
    id: "r1",
    sessionId: "s1",
    turnId: "t1",
    kind: "approval",
    title: "Allow Bash?",
    detailJson: JSON.stringify({ command: "rm -rf build" }),
    optionsJson: JSON.stringify([
      { id: "allow", label: "Allow once" },
      { id: "deny", label: "Deny" },
    ]),
    status: "pending",
    createdAtMs: 1,
    ...overrides,
  };
}

describe("requestDetail", () => {
  it("parses what the agent proposed", () => {
    expect(requestDetail(request())).toEqual({ command: "rm -rf build" });
  });

  it("yields no detail rather than throwing on malformed JSON", () => {
    // A dialog that cannot render is worse than one that renders without the
    // command: the agent is blocked either way, and only one of the two lets the
    // user unblock it.
    expect(requestDetail(request({ detailJson: "{not json" }))).toEqual({});
  });

  it("treats a non-object payload as no detail", () => {
    // `JSON.parse("null")` succeeds and returns null, which would otherwise reach
    // the dialog as an object and throw on property access.
    expect(requestDetail(request({ detailJson: "null" }))).toEqual({});
    expect(requestDetail(request({ detailJson: '"a string"' }))).toEqual({});
    expect(requestDetail(request({ detailJson: "42" }))).toEqual({});
  });

  it("passes an empty object through", () => {
    expect(requestDetail(request({ detailJson: "{}" }))).toEqual({});
  });
});

describe("requestOptions", () => {
  it("returns the options the agent offered, in order", () => {
    const options = requestOptions(request());
    expect(options.map((option) => option.id)).toEqual(["allow", "deny"]);
    expect(options[0].label).toBe("Allow once");
  });

  it("preserves a persistent grant's flag", () => {
    // "Always allow" is a policy change rather than one decision, and the dialog
    // styles it differently on the strength of this flag.
    const options = requestOptions(
      request({
        optionsJson: JSON.stringify([
          { id: "allow_always", label: "Always", persistent: true },
        ]),
      }),
    );
    expect(options[0].persistent).toBe(true);
  });

  it("falls back to allow and deny when the payload is unusable", () => {
    // Without a fallback the dialog renders with no buttons, leaving the agent
    // blocked and the user with nothing to press. The backend validates whatever
    // is sent against what the agent actually offered, so a wrong guess is
    // refused rather than silently accepted.
    for (const optionsJson of ["{not json", "[]", "null", '{"id":"allow"}']) {
      const options = requestOptions(request({ optionsJson }));
      expect(options.map((option) => option.id)).toEqual(["allow", "deny"]);
    }
  });
});
