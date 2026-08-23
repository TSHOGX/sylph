"use client";

/**
 * Unified inline diff for an agent edit.
 *
 * Unified rather than side-by-side: the Artifacts pane already takes horizontal
 * space, and a split view in the remaining width wraps every line. Unified also
 * matches what `git diff` shows, so the rendering is already familiar.
 *
 * Both line numbers are shown because an edit's value is knowing where in the
 * file it landed, and a single-column number is ambiguous once lines are added.
 */

import { memo, useMemo, useState } from "react";
import { collapseContext, diffLines, type DiffLine } from "@/lib/diff";

/** Changed lines beyond which the diff opens collapsed. */
const LARGE_DIFF_LINES = 120;

export const DiffView = memo(function DiffView({
  before,
  after,
  path,
  onOpenFile,
}: {
  before: string;
  after: string;
  path?: string;
  /** Called when the header path is clicked, to show the file in Artifacts. */
  onOpenFile?: (path: string) => void;
}) {
  const result = useMemo(() => diffLines(before, after), [before, after]);
  const changed = result.added + result.removed;
  const [expanded, setExpanded] = useState(changed <= LARGE_DIFF_LINES);

  // Unchanged runs are elided with a gap marker, the way a unified diff's `@@`
  // hunks do. Without this, a three-line change inside a 400-line replacement
  // renders 400 rows to show three.
  const segments = useMemo(() => collapseContext(result.lines), [result.lines]);

  if (result.lines.length === 0) {
    return (
      <p className="rounded border border-line bg-sunken px-2 py-1 text-2xs text-ink-muted">
        No textual change.
      </p>
    );
  }

  return (
    <div className="overflow-hidden rounded border border-line">
      <div className="flex items-center justify-between gap-2 border-b border-line bg-sunken px-2 py-0.5 text-3xs">
        <div className="flex min-w-0 items-center gap-2">
          {path && (
            <button
              type="button"
              onClick={() => onOpenFile?.(path)}
              disabled={!onOpenFile}
              title={onOpenFile ? `Open ${path} in Artifacts` : path}
              className="truncate font-mono text-ink-muted hover:text-ink hover:underline focus-ring disabled:hover:no-underline"
            >
              {path}
            </button>
          )}
          <span className="shrink-0 font-medium">
            <span className="text-success">+{result.added}</span>{" "}
            <span className="text-danger">−{result.removed}</span>
          </span>
          {result.degraded && (
            <span
              className="shrink-0 text-warning"
              title="The change was too large to align line by line, so it is shown as a full replacement."
            >
              unaligned
            </span>
          )}
        </div>
        {changed > LARGE_DIFF_LINES && (
          <button
            type="button"
            onClick={() => setExpanded(!expanded)}
            className="shrink-0 rounded px-1 font-medium text-ink-muted hover:bg-raised hover:text-ink focus-ring"
          >
            {expanded ? "Collapse" : `Show ${changed} changed lines`}
          </button>
        )}
      </div>

      {expanded && (
        <div className="overflow-x-auto bg-raised">
          <table className="w-full border-collapse font-mono text-2xs leading-[1.55]">
            {/* A table gives the gutter columns a shared width without
                measuring, and keeps each line's numbers aligned with its text. */}
            <tbody>
              {segments.map((segment, segmentIndex) => (
                <DiffSegment
                  key={segmentIndex}
                  lines={segment.lines}
                  skipped={segmentIndex === 0 ? 0 : segment.skippedBefore}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
});

function DiffSegment({ lines, skipped }: { lines: DiffLine[]; skipped: number }) {
  return (
    <>
      {skipped > 0 && (
        <tr>
          <td colSpan={3} className="bg-sunken px-2 py-px text-3xs text-ink-muted">
            ⋯ {skipped} unchanged {skipped === 1 ? "line" : "lines"}
          </td>
        </tr>
      )}
      {lines.map((line, index) => (
        <DiffRow key={index} line={line} />
      ))}
    </>
  );
}

function DiffRow({ line }: { line: DiffLine }) {
  const tone =
    line.op === "add"
      ? "bg-success-bg text-success"
      : line.op === "remove"
        ? "bg-danger-bg text-danger"
        : "text-ink-soft";

  // The sign is real text, not a background colour alone: colour-blind readers
  // and monochrome printing both need it (WCAG 1.4.1).
  const sign = line.op === "add" ? "+" : line.op === "remove" ? "−" : " ";

  return (
    <tr className={tone}>
      <td
        aria-hidden="true"
        className="w-10 select-none border-r border-line px-1 text-right align-top text-ink-muted"
      >
        {line.beforeLine ?? ""}
      </td>
      <td
        aria-hidden="true"
        className="w-10 select-none border-r border-line px-1 text-right align-top text-ink-muted"
      >
        {line.afterLine ?? ""}
      </td>
      <td className="whitespace-pre px-2 align-top">
        <span aria-hidden="true" className="select-none opacity-60">
          {sign}
        </span>
        {/* A screen reader gets the operation as words, since the glyph alone
            reads as punctuation. */}
        {line.op !== "context" && (
          <span className="sr-only">{line.op === "add" ? "added: " : "removed: "}</span>
        )}
        {line.text || " "}
      </td>
    </tr>
  );
}
