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

/**
 * Whether a lasting grant may be offered.
 *
 * Not cosmetic. An agent marks the asks where granting "always" would write a
 * permission rule broader than the ask itself, and showing it anyway turns a narrow
 * yes into a wide one the user never read. The filtering lives in `requestOptions`
 * so every consumer inherits it rather than each dialog having to remember.
 */
describe("requestOptions and a lasting grant", () => {
  const withPersistent = JSON.stringify([
    { id: "allow", label: "Allow once" },
    { id: "allow_always", label: "Always allow Bash", persistent: true },
    { id: "deny", label: "Deny" },
  ]);

  it("offers a persistent option when the request permits one", () => {
    const options = requestOptions(
      request({ optionsJson: withPersistent, allowPersistent: true }),
    );
    expect(options.map((option) => option.id)).toEqual([
      "allow",
      "allow_always",
      "deny",
    ]);
  });

  it("omits the persistent option when the request forbids one", () => {
    const options = requestOptions(
      request({ optionsJson: withPersistent, allowPersistent: false }),
    );
    expect(options.map((option) => option.id)).toEqual(["allow", "deny"]);
    expect(options.some((option) => option.persistent)).toBe(false);
  });

  it("treats an absent flag as permitting one, so a minimal adapter still works", () => {
    const options = requestOptions(request({ optionsJson: withPersistent }));
    expect(options.map((option) => option.id)).toContain("allow_always");
  });

  it("still offers a way to decline when every option was persistent", () => {
    // Filtering must not empty the dialog. A panel with no buttons leaves the agent
    // blocked with no way for the user to answer at all, which is strictly worse
    // than the over-broad grant the filter exists to avoid.
    const options = requestOptions(
      request({
        optionsJson: JSON.stringify([
          { id: "allow_always", label: "Always", persistent: true },
        ]),
        allowPersistent: false,
      }),
    );
    expect(options).toEqual([{ id: "deny", label: "Deny" }]);
  });
});
