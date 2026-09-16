import { expect, it } from "vitest";
import { bodyWindow, nextBodyStart } from "./trace-ranges";
import type { PayloadAccess } from "./trace-types";

const partial = {
  availableRanges: [{ start: 100, endExclusive: 112 }, { start: 200, endExclusive: 212 }],
  totalBytes: 500,
  availability: { kind: "partial", reason: "Only these intervals were retained" },
} satisfies Pick<PayloadAccess, "availableRanges" | "totalBytes" | "availability">;

it("opens the first observed interval and keeps UTF-8 overlap inside retained bytes", () => {
  expect(bodyWindow(partial, 0, 8, 3)).toEqual({
    start: 100, sourceStart: 100, end: 108, rangeEnd: 112, nextRange: 200, previous: null,
  });
  const second = bodyWindow(partial, 107, 8, 3)!;
  expect(second.sourceStart).toBe(104);
  expect(second.end).toBe(112);
  expect(second.end - second.sourceStart).toBeLessThanOrEqual(8);
});

it("moves across holes in both directions without requesting the omitted bytes", () => {
  const first = bodyWindow(partial, 107, 8, 3)!;
  expect(nextBodyStart(first, 112, 500)).toBe(200);
  const later = bodyWindow(partial, 200, 8, 3)!;
  expect(later.previous).toBe(104);
  const last = bodyWindow(partial, 207, 8, 3)!;
  expect(nextBodyStart(last, 212, 500)).toBeNull();
});

it("does not invent a range for a partial observation with no retained intervals", () => {
  expect(bodyWindow({ ...partial, availableRanges: [] }, 0, 8, 3)).toBeNull();
});

it("keeps a complete unknown-length body bounded and stops at the returned length", () => {
  const current = bodyWindow({ availableRanges: [], totalBytes: null, availability: { kind: "available" } }, 0, 8, 3)!;
  expect(current.end).toBe(8);
  expect(nextBodyStart(current, 8, null)).toBe(8);
  expect(nextBodyStart(current, 5, 5)).toBeNull();
});

it("treats adjacent retained intervals as one byte span for multibyte decoding", () => {
  const current = bodyWindow({ ...partial, availableRanges: [{ start: 0, endExclusive: 3 }, { start: 3, endExclusive: 8 }] }, 0, 8, 3)!;
  expect(current.end).toBe(8);
  expect(current.rangeEnd).toBe(8);
  expect(current.nextRange).toBeNull();
});
