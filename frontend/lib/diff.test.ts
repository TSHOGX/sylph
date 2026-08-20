import { describe, expect, it } from "vitest";
import { collapseContext, diffLines } from "@/lib/diff";

/**
 * The diff is what makes an agent edit legible, so these pin the properties a
 * reader depends on: line numbers that refer to the real file, a `-` before the
 * matching `+` for a replaced line, and no phantom trailing line.
 */
describe("diffLines", () => {
  it("reports no change for identical text", () => {
    const result = diffLines("a\nb\n", "a\nb\n");
    expect(result.lines).toEqual([]);
    expect(result.added).toBe(0);
    expect(result.removed).toBe(0);
  });

  it("treats an empty before as a file creation", () => {
    const result = diffLines("", "one\ntwo\n");
    expect(result.added).toBe(2);
    expect(result.removed).toBe(0);
    expect(result.lines.map((line) => line.op)).toEqual(["add", "add"]);
    // Additions carry an after-line number and no before-line number, which is
    // what lets the renderer leave the left gutter empty.
    expect(result.lines[0].afterLine).toBe(1);
    expect(result.lines[0].beforeLine).toBeUndefined();
  });

  it("treats an empty after as a deletion", () => {
    const result = diffLines("one\ntwo\n", "");
    expect(result.removed).toBe(2);
    expect(result.added).toBe(0);
    expect(result.lines.every((line) => line.op === "remove")).toBe(true);
  });

  it("emits the removal before the addition for a replaced line", () => {
    const result = diffLines("fn main() {\n  old();\n}\n", "fn main() {\n  new();\n}\n");
    expect(result.added).toBe(1);
    expect(result.removed).toBe(1);
    const ops = result.lines.map((line) => `${line.op}:${line.text.trim()}`);
    expect(ops).toEqual([
      "context:fn main() {",
      "remove:old();",
      "add:new();",
      "context:}",
    ]);
  });

  it("numbers lines against the original files, not the changed region", () => {
    const before = "1\n2\n3\n4\n5\n";
    const after = "1\n2\nX\n4\n5\n";
    const changed = diffLines(before, after).lines.filter((l) => l.op !== "context");
    expect(changed).toHaveLength(2);
    // The edit is on line 3 of both sides, and stays reported as line 3 even
    // though the aligned slice began at index 2.
    expect(changed[0]).toMatchObject({ op: "remove", text: "3", beforeLine: 3 });
    expect(changed[1]).toMatchObject({ op: "add", text: "X", afterLine: 3 });
  });

  it("keeps line numbers correct when a line is inserted", () => {
    const result = diffLines("a\nb\n", "a\nnew\nb\n");
    const added = result.lines.find((line) => line.op === "add");
    expect(added).toMatchObject({ text: "new", afterLine: 2 });
    // `b` is unchanged but moves: line 2 before, line 3 after. Reporting both is
    // why the renderer shows two gutters.
    const shifted = result.lines.find((line) => line.text === "b");
    expect(shifted).toMatchObject({ op: "context", beforeLine: 2, afterLine: 3 });
  });

  it("does not invent a trailing blank line from the final newline", () => {
    // `"a\n".split("\n")` yields `["a", ""]`; without handling that, every file
    // would show a phantom empty last line and compare unequal to one without
    // the newline.
    // Differing only in a trailing newline is not a line-level change, so it is
    // reported as none rather than as a table of unmarked context rows.
    const result = diffLines("a\n", "a");
    expect(result.lines).toEqual([]);
    expect(result.added + result.removed).toBe(0);
    expect(diffLines("a\nb\n", "a\nb").added).toBe(0);
  });

  it("aligns a change buried in a large identical body cheaply", () => {
    const body = Array.from({ length: 500 }, (_, i) => `line ${i}`);
    const before = body.join("\n");
    const after = [...body.slice(0, 250), "CHANGED", ...body.slice(251)].join("\n");

    const started = Date.now();
    const result = diffLines(before, after);
    const elapsed = Date.now() - started;

    expect(result.added).toBe(1);
    expect(result.removed).toBe(1);
    expect(result.degraded).toBe(false);
    // Prefix/suffix stripping is what keeps this off the quadratic path. A
    // generous bound: the point is that it is not seconds.
    expect(elapsed).toBeLessThan(200);
  });

  it("degrades to a wholesale replacement rather than hanging on a huge change", () => {
    // Both sides differ entirely and exceed the alignment cap, so the honest
    // answer is "all removed, all added" instead of a multi-second DP table.
    const before = Array.from({ length: 2500 }, (_, i) => `a${i}`).join("\n");
    const after = Array.from({ length: 2500 }, (_, i) => `b${i}`).join("\n");

    const started = Date.now();
    const result = diffLines(before, after);
    const elapsed = Date.now() - started;

    expect(result.degraded).toBe(true);
    expect(result.removed).toBe(2500);
    expect(result.added).toBe(2500);
    expect(elapsed).toBeLessThan(500);
  });

  it("handles text with no trailing newline on either side", () => {
    const result = diffLines("only", "only changed");
    expect(result.added).toBe(1);
    expect(result.removed).toBe(1);
  });
});

describe("collapseContext", () => {
  it("keeps context around each change and reports the elided count", () => {
    const lines = diffLines(
      Array.from({ length: 40 }, (_, i) => `line ${i}`).join("\n"),
      [
        ...Array.from({ length: 20 }, (_, i) => `line ${i}`),
        "CHANGED",
        ...Array.from({ length: 19 }, (_, i) => `line ${i + 21}`),
      ].join("\n"),
    ).lines;

    const segments = collapseContext(lines, 3);
    const shown = segments.flatMap((segment) => segment.lines);

    // Only the changed lines plus three of context on each side survive, so a
    // one-line edit in a 40-line block does not render 40 rows.
    expect(shown.length).toBeLessThan(12);
    expect(shown.some((line) => line.text === "CHANGED")).toBe(true);
    // The first segment starts mid-file, so the renderer needs to know how many
    // lines it skipped to draw the gap marker.
    expect(segments[0].skippedBefore).toBeGreaterThan(0);
  });

  it("returns a single segment when everything changed", () => {
    const lines = diffLines("a\n", "b\n").lines;
    const segments = collapseContext(lines);
    expect(segments).toHaveLength(1);
    expect(segments[0].skippedBefore).toBe(0);
  });
});
