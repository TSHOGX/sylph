import { describe, expect, it } from "vitest";
import { formatDuration } from "@/lib/format";

/**
 * Duration rendering, shared by the timeline card and the delegation overview.
 *
 * The bug this file exists to prevent: those two surfaces each carried their own copy,
 * identical but for `Math.round` on the sub-second branch — so one rendered `1.5ms` where
 * the other rendered `2ms`, for the *same* delegation. They are the two places a reader
 * compares one node's duration against another's, which is exactly where a disagreement
 * costs something.
 */
describe("formatDuration", () => {
  it("rounds sub-second durations to whole milliseconds", () => {
    // The regression: a fractional input must render identically wherever it is shown.
    expect(formatDuration(1.5)).toBe("2ms");
    expect(formatDuration(0.4)).toBe("0ms");
    expect(formatDuration(999.6)).toBe("1000ms");
    expect(formatDuration(42)).toBe("42ms");
  });

  it("treats zero as a real duration, not a missing one", () => {
    // A delegation whose start and close land in the same millisecond took under 1ms.
    // Whether a span is *knowable* is decided before this is called.
    expect(formatDuration(0)).toBe("0ms");
  });

  it("switches to seconds at one second, with one decimal", () => {
    expect(formatDuration(1000)).toBe("1.0s");
    expect(formatDuration(1500)).toBe("1.5s");
    expect(formatDuration(59_940)).toBe("59.9s");
  });

  it("switches to minutes and seconds at one minute", () => {
    expect(formatDuration(60_000)).toBe("1m 0s");
    expect(formatDuration(90_000)).toBe("1m 30s");
    expect(formatDuration(3_600_000)).toBe("60m 0s");
  });
});
