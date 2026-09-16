import { describe, expect, it } from "vitest";
import { extendTraceWindow, tracePageButtonDisabled } from "./trace-window";
import type { ReadingUnit, TracePage } from "./trace-types";

function page(start: number, end: number): TracePage<ReadingUnit> {
  return {
    view: {
      token: "leased-view",
      generation: "generation",
      evidenceCut: 5,
      viewRevision: 5,
      expiresAtMs: 1000,
    },
    items: Array.from(
      { length: end - start },
      (_, index) =>
        ({
          address: `entry-${start + index}`,
          anchor: `object-${start + index}`,
        }) as ReadingUnit,
    ),
    beforeCursor: start ? `before-${start}` : null,
    afterCursor: `after-${end}`,
    hasBefore: start > 0,
    hasAfter: true,
    total: { value: 1000, scope: "Agent", complete: true },
    state: "more",
    returnedItems: end - start,
    returnedBytes: 100,
    limits: { maxItems: 2 } as TracePage<ReadingUnit>["limits"],
  };
}

describe("consistent bounded reading windows", () => {
  it("rejects a newer view instead of combining incompatible history", () => {
    const next = page(2, 4);
    next.view = { ...next.view, token: "new-lease", viewRevision: 6 };
    expect(() => extendTraceWindow(page(0, 2), next, "after")).toThrow(
      "History changed",
    );
  });
  it("evicts the far page while keeping exact cursor boundaries", () => {
    const current = extendTraceWindow(page(0, 2), page(2, 4), "after");
    const next = extendTraceWindow(current, page(4, 6), "after");
    expect(next.items.map((item) => item.address)).toEqual([
      "entry-2",
      "entry-3",
      "entry-4",
      "entry-5",
    ]);
    expect(next.beforeCursor).toBe("before-2");
    expect(next.afterCursor).toBe("after-6");
    expect(next.windows).toHaveLength(2);
  });
  it("prepends in source order and keeps one address for overlapping rows", () => {
    const next = extendTraceWindow(page(2, 4), page(1, 3), "before");
    expect(next.items.map((item) => item.address)).toEqual([
      "entry-1",
      "entry-2",
      "entry-3",
    ]);
    expect(next.beforeCursor).toBe("before-1");
    expect(next.afterCursor).toBe("after-4");
  });
  it("keeps a failed detail boundary retryable", () => {
    expect(tracePageButtonDisabled(false, true, undefined, true)).toBe(false);
    expect(tracePageButtonDisabled(false, false, false, true)).toBe(true);
    expect(tracePageButtonDisabled(true, true, true, true)).toBe(true);
  });
});
