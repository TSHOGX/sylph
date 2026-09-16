import { describe, expect, it } from "vitest";
import { commandControl } from "./trace-command";
import type { ControlAvailability } from "./trace-types";
const displayed: Extract<ControlAvailability, { kind: "available" }> = {
  kind: "available",
  target: {
    kind: "embedded",
    agent: "child",
    hostSessionId: "host",
    namespace: "vendor.child",
    nativeChildId: "child-native",
    route: "child-route",
  },
  actions: ["send", "stop", "steer"],
  lease: "lease-one",
  affectedExecution: "execution-one",
};

describe("typed command targeting", () => {
  it("rejects stopping a newer execution even when the same agent remains controllable", () => {
    expect(() =>
      commandControl("child", "stop", displayed, {
        ...displayed,
        lease: "lease-two",
        affectedExecution: "execution-two",
      }),
    ).toThrow("targeted execution changed");
  });
  it("keeps the native selector and displayed lease when the guarded target is unchanged", () => {
    expect(commandControl("child", "steer", displayed, { ...displayed })).toBe(
      displayed,
    );
  });
  it("does not replace an embedded recipient with its host session", () => {
    expect(() =>
      commandControl("child", "send", displayed, {
        ...displayed,
        target: { kind: "session", agent: "host", sessionId: "host" },
      }),
    ).toThrow("no longer available");
  });
  it("uses freshly authorized availability for an independent new input", () => {
    const fresh = { ...displayed, lease: "new-current-lease" };
    expect(commandControl("child", "send", displayed, fresh)).toBe(fresh);
    expect(() =>
      commandControl("child", "send", displayed, {
        kind: "unavailable",
        reason: "Control access removed",
      }),
    ).toThrow("Control access removed");
  });
});
