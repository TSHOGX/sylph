import { describe, expect, it } from "vitest";
import { turnDurationMs } from "./turn-duration";
import type { Turn } from "./api-types";

const turn = (overrides: Partial<Turn> = {}): Turn => ({
  id: "t", sessionId: "s", status: "completed", origin: "user", requestText: "",
  commandInvocationId: null, idempotencyKey: null, errorCode: null, errorMessage: null,
  createdAtMs: 1, startedAtMs: 100, completedAtMs: 250, stoppedAtMs: null, ...overrides,
});

describe("turnDurationMs", () => {
  it("only measures stream records with ordered endpoints", () => {
    expect(turnDurationMs(turn(), "stream")).toBe(150);
    expect(turnDurationMs(turn(), "blocks")).toBeUndefined();
    expect(turnDurationMs(turn({ completedAtMs: null }), "stream")).toBeUndefined();
    expect(turnDurationMs(turn({ completedAtMs: 50 }), "stream")).toBeUndefined();
  });
});
