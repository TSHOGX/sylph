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
      <p className="rounded border border-gray-200 bg-gray-50 px-2 py-1 text-[11px] text-gray-500">
        No textual change.
      </p>
    );
  }

  return (
    <div className="overflow-hidden rounded border border-gray-200">
      <div className="flex items-center justify-between gap-2 border-b border-gray-200 bg-gray-50 px-2 py-0.5 text-[10px]">
        <div className="flex min-w-0 items-center gap-2">
          {path && (
            <button
              type="button"
              onClick={() => onOpenFile?.(path)}
              disabled={!onOpenFile}
              title={onOpenFile ? `Open ${path} in Artifacts` : path}
              className="truncate font-mono text-gray-600 hover:text-gray-900 hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:hover:no-underline"
            >
              {path}
            </button>
          )}
          <span className="shrink-0 font-medium">
            <span className="text-emerald-700">+{result.added}</span>{" "}
            <span className="text-red-700">−{result.removed}</span>
          </span>
          {result.degraded && (
            <span
              className="shrink-0 text-amber-700"
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
            className="shrink-0 rounded px-1 font-medium text-gray-600 hover:bg-white hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            {expanded ? "Collapse" : `Show ${changed} changed lines`}
          </button>
        )}
      </div>

      {expanded && (
        <div className="overflow-x-auto bg-white">
          <table className="w-full border-collapse font-mono text-[11.5px] leading-[1.55]">
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
          <td colSpan={3} className="bg-gray-50 px-2 py-px text-[10px] text-gray-500">
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
      ? "bg-emerald-50 text-emerald-950"
      : line.op === "remove"
        ? "bg-red-50 text-red-950"
        : "text-gray-700";

  // The sign is real text, not a background colour alone: colour-blind readers
  // and monochrome printing both need it (WCAG 1.4.1).
  const sign = line.op === "add" ? "+" : line.op === "remove" ? "−" : " ";

  return (
    <tr className={tone}>
      <td
        aria-hidden="true"
        className="w-10 select-none border-r border-gray-100 px-1 text-right align-top text-gray-500"
      >
        {line.beforeLine ?? ""}
      </td>
      <td
        aria-hidden="true"
        className="w-10 select-none border-r border-gray-100 px-1 text-right align-top text-gray-500"
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
