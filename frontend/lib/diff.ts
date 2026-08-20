/**
 * Line-level diff for rendering agent edits inline.
 *
 * # Why compute this here
 *
 * Agents report edits as an opaque before/after string pair, not as a diff. To
 * show *what changed* rather than two blobs, the diff has to be computed
 * somewhere, and the browser is the only place that has both strings.
 *
 * # Why no dependency
 *
 * `diff` (jsdiff) is ~30 KB minified and `diff-match-patch` ~70 KB. Phase 4
 * measured first-load JS at 190.4 KB against a 200 KB budget, so either would
 * blow it on its own. What is needed here is line-granularity Myers on inputs
 * that are almost always under a few hundred lines — a bounded, well-understood
 * algorithm. The library's value is character-level diffing and patch formats,
 * neither of which this UI renders.
 *
 * # Algorithm
 *
 * Classic LCS via dynamic programming, with the two optimizations that matter in
 * practice: common prefix/suffix are stripped first (an agent edit usually
 * changes a handful of lines in a familiar block), and inputs above a size cap
 * fall back to "replace wholesale" rather than allocating a quadratic table. The
 * cap exists because the DP table is O(n·m): 2000×2000 is 4M cells, which is
 * fine, while 50000×50000 would hang the tab.
 */

export type DiffOp = "context" | "add" | "remove";

export interface DiffLine {
  op: DiffOp;
  text: string;
  /** 1-based line number in the "before" text. Absent for additions. */
  beforeLine?: number;
  /** 1-based line number in the "after" text. Absent for removals. */
  afterLine?: number;
}

export interface DiffResult {
  lines: DiffLine[];
  added: number;
  removed: number;
  /** True when the inputs exceeded [`MAX_DIFF_LINES`] and were not aligned. */
  degraded: boolean;
}

/**
 * Largest input, in lines per side, that gets a real diff.
 *
 * At this size the DP table is 4M `Uint32` cells (16 MB) and completes in well
 * under the frame budget on a laptop. Beyond it, the honest thing is to say the
 * change was too large to align rather than to freeze the tab computing an
 * alignment nobody will read line by line.
 */
const MAX_DIFF_LINES = 2000;

export function diffLines(before: string, after: string): DiffResult {
  if (before === after) {
    return { lines: [], added: 0, removed: 0, degraded: false };
  }

  const beforeLines = splitLines(before);
  const afterLines = splitLines(after);

  // Texts differing only in a trailing newline split to the same lines. Reporting
  // that as a diff would render a table of context rows with nothing marked, so
  // it is reported as no change — which is what it is, line-wise.
  if (
    beforeLines.length === afterLines.length &&
    beforeLines.every((line, index) => line === afterLines[index])
  ) {
    return { lines: [], added: 0, removed: 0, degraded: false };
  }

  if (before.length === 0) {
    return {
      lines: afterLines.map((text, index) => ({ op: "add", text, afterLine: index + 1 })),
      added: afterLines.length,
      removed: 0,
      degraded: false,
    };
  }
  if (after.length === 0) {
    return {
      lines: beforeLines.map((text, index) => ({ op: "remove", text, beforeLine: index + 1 })),
      added: 0,
      removed: beforeLines.length,
      degraded: false,
    };
  }

  // Strip the identical head and tail. This is what makes a one-line change in a
  // 500-line block cost almost nothing, and it shrinks the DP table to the part
  // that actually differs.
  let head = 0;
  while (
    head < beforeLines.length &&
    head < afterLines.length &&
    beforeLines[head] === afterLines[head]
  ) {
    head += 1;
  }
  let tail = 0;
  while (
    tail < beforeLines.length - head &&
    tail < afterLines.length - head &&
    beforeLines[beforeLines.length - 1 - tail] === afterLines[afterLines.length - 1 - tail]
  ) {
    tail += 1;
  }

  const beforeMiddle = beforeLines.slice(head, beforeLines.length - tail);
  const afterMiddle = afterLines.slice(head, afterLines.length - tail);

  const degraded =
    beforeMiddle.length > MAX_DIFF_LINES || afterMiddle.length > MAX_DIFF_LINES;

  const middle: DiffLine[] = degraded
    ? [
        // No alignment attempted: report the change as a wholesale replacement,
        // which is accurate, just less precise.
        ...beforeMiddle.map((text, index) => ({
          op: "remove" as const,
          text,
          beforeLine: head + index + 1,
        })),
        ...afterMiddle.map((text, index) => ({
          op: "add" as const,
          text,
          afterLine: head + index + 1,
        })),
      ]
    : align(beforeMiddle, afterMiddle, head);

  const lines: DiffLine[] = [
    ...beforeLines.slice(0, head).map((text, index) => ({
      op: "context" as const,
      text,
      beforeLine: index + 1,
      afterLine: index + 1,
    })),
    ...middle,
    ...beforeLines.slice(beforeLines.length - tail).map((text, index) => ({
      op: "context" as const,
      text,
      beforeLine: beforeLines.length - tail + index + 1,
      afterLine: afterLines.length - tail + index + 1,
    })),
  ];

  return {
    lines,
    added: lines.filter((line) => line.op === "add").length,
    removed: lines.filter((line) => line.op === "remove").length,
    degraded,
  };
}

/**
 * LCS alignment of the differing middle sections.
 *
 * `offset` is how many identical lines were stripped from the head, so emitted
 * line numbers refer to the original files rather than to the slices.
 */
function align(before: string[], after: string[], offset: number): DiffLine[] {
  const n = before.length;
  const m = after.length;

  // Row-major (n+1)×(m+1) LCS lengths. A typed array rather than nested arrays:
  // one allocation, and contiguous access on the inner loop.
  const width = m + 1;
  const table = new Uint32Array((n + 1) * width);

  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      table[i * width + j] =
        before[i] === after[j]
          ? table[(i + 1) * width + (j + 1)] + 1
          : Math.max(table[(i + 1) * width + j], table[i * width + (j + 1)]);
    }
  }

  const lines: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (before[i] === after[j]) {
      lines.push({
        op: "context",
        text: before[i],
        beforeLine: offset + i + 1,
        afterLine: offset + j + 1,
      });
      i += 1;
      j += 1;
    } else if (table[(i + 1) * width + j] >= table[i * width + (j + 1)]) {
      // Emitting the removal first groups a replaced line as `-` then `+`,
      // which is how a unified diff reads.
      lines.push({ op: "remove", text: before[i], beforeLine: offset + i + 1 });
      i += 1;
    } else {
      lines.push({ op: "add", text: after[j], afterLine: offset + j + 1 });
      j += 1;
    }
  }
  while (i < n) {
    lines.push({ op: "remove", text: before[i], beforeLine: offset + i + 1 });
    i += 1;
  }
  while (j < m) {
    lines.push({ op: "add", text: after[j], afterLine: offset + j + 1 });
    j += 1;
  }

  return lines;
}

/**
 * Split into lines without inventing a trailing empty one.
 *
 * `"a\n".split("\n")` yields `["a", ""]`, which would render a phantom blank
 * line at the end of every file and report it as changed when the other side
 * lacks the newline.
 */
function splitLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/**
 * Drop long runs of unchanged lines, keeping `context` lines around each change.
 *
 * A diff of a 500-line replacement is unreadable and slow to render; a unified
 * diff's `@@` hunks exist for exactly this reason. Returns segments so the
 * renderer can draw a gap marker between them.
 */
export function collapseContext(
  lines: DiffLine[],
  context = 3,
): Array<{ lines: DiffLine[]; skippedBefore: number }> {
  const keep = new Set<number>();
  lines.forEach((line, index) => {
    if (line.op === "context") return;
    for (let at = index - context; at <= index + context; at += 1) {
      if (at >= 0 && at < lines.length) keep.add(at);
    }
  });

  const segments: Array<{ lines: DiffLine[]; skippedBefore: number }> = [];
  let current: DiffLine[] = [];
  let skipped = 0;

  lines.forEach((line, index) => {
    if (keep.has(index)) {
      current.push(line);
    } else if (current.length > 0) {
      segments.push({ lines: current, skippedBefore: skipped });
      current = [];
      skipped = 1;
    } else {
      skipped += 1;
    }
  });
  if (current.length > 0) segments.push({ lines: current, skippedBefore: skipped });

  return segments;
}
