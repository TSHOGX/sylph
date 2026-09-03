import { describe, expect, it } from "vitest";
import { mergeInteriorState } from "@/lib/interior-state";

describe("mergeInteriorState", () => {
  it("keeps the strongest source evidence regardless of arrival order", () => {
    expect(mergeInteriorState("pending", "partial")).toBe("partial");
    expect(mergeInteriorState("partial", "pending")).toBe("partial");
    expect(mergeInteriorState("partial", "full")).toBe("full");
    expect(mergeInteriorState("full", "pending")).toBe("full");
  });

  it("does not collapse terminal capability and diagnostic states into full", () => {
    expect(mergeInteriorState("pending", "forwards_nothing")).toBe("forwards_nothing");
    expect(mergeInteriorState("pending", "unreadable")).toBe("unreadable");
    expect(mergeInteriorState("unreadable", "pending")).toBe("unreadable");
  });
});
