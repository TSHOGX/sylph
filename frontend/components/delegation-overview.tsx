"use client";

/**
 * One time axis across every delegation in view: which of these agents was slow.
 *
 * # Why this is not a chart
 *
 * `lib/timeline-waterfall.ts` explains the question and the honesty rules; this file
 * is the rendering, and it is deliberately two `div`s per row. A bar is
 * `left: {offset}%; width: {length}%` against a shared origin, so it is responsive
 * with no measuring pass, no `<canvas>`, no ResizeObserver, and no library — the
 * cheapest chart library on npm would cost more than the whole first-load headroom
 * this route has left.
 *
 * # Why it is a list of buttons and not a picture
 *
 * A purely visual overview excludes the reader who most needs a summary. Every row is
 * a real control whose accessible name carries what the bar shows — name, status,
 * elapsed time, when it started relative to the turn, and how deep it sits — so the
 * comparison is available without seeing the bars at all. The bar itself is
 * `aria-hidden`: it is a second rendering of the label, not new information.
 *
 * Native tab order rather than a roving cursor. These are navigation targets like the
 * breadcrumb's, not options in a choice list, and `lib/select-list.ts` exists so that
 * a *choice list* does not grow a fourth cursor — adopting it here would mean
 * pretending a delegation is an option being picked, and `aria-selected` would then
 * claim a selection nobody made.
 *
 * # Why it is closed by default
 *
 * It costs one row per trajectory *in the whole subtree*, which is unbounded — a real
 * workflow reaches a hundred. Closed, it renders a button; open, it renders what the
 * user asked for. That is the same trade the cards make, and the toggle states the
 * count so opening it is an informed choice rather than a surprise.
 */

import { useMemo, useState } from "react";
import {
  trajectoryName,
  trajectoryReport,
  type TimelineNode,
  type TrajectoryStatus,
} from "@/lib/timeline-tree";
import {
  buildWaterfall,
  type WaterfallOrder,
  type WaterfallRow,
} from "@/lib/timeline-waterfall";

/** Bar fill per status. Never the only carrier of the status — the label says it too. */
const BAR_TONE: Record<TrajectoryStatus, string> = {
  running: "bg-info-solid/70",
  done: "bg-success-solid/60",
  failed: "bg-danger-solid/70",
  // Warn, not danger: nobody said it failed, only that nobody said.
  abandoned: "bg-warning-solid/60",
};

const STATUS_WORD: Record<TrajectoryStatus, string> = {
  running: "running",
  done: "finished",
  failed: "failed",
  abandoned: "never finished",
};

/**
 * How far a label indents before depth becomes a badge instead.
 *
 * Same reasoning and same number as the timeline's `MAX_INDENT`, for the same reason:
 * past two levels a 412px viewport has no width left for the name, which is the one
 * thing the row cannot lose. The *bars* never indent at all — they share an axis, and
 * shifting them by depth would destroy the only comparison this view makes.
 */
const MAX_LABEL_INDENT = 2;

export function DelegationOverview({
  nodes,
  onFocus,
}: {
  nodes: TimelineNode[];
  onFocus: (trajectory: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [order, setOrder] = useState<WaterfallOrder>("structure");

  // Keyed on the node array, which `buildTree` reallocates per build — so this
  // recomputes per token while a turn streams. That is affordable and deliberate:
  // the work is O(trajectories), not O(events), and the axis *must* move as events
  // arrive or a running bar would stop growing. Keying it on something stabler is
  // the trap the `NODE_CACHE` pitfall describes from the other side: a key the
  // producer reallocates can never hit, and here there is nothing stabler to key on
  // because the value genuinely changes.
  const waterfall = useMemo(() => buildWaterfall(nodes, order), [nodes, order]);

  // One delegation compares to nothing. The card already reports its duration, so an
  // overview here would be a second copy of one number.
  if (waterfall.rows.length < 2) return null;

  const count = waterfall.rows.length;

  return (
    <section aria-label="Delegation overview" className="rounded bg-raised/40">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-1.5 py-1">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          className="tap-row rounded px-1 text-2xs font-medium text-ink-soft hover:bg-hover hover:text-ink focus-ring"
        >
          {open ? "Hide" : "Compare"} {count} delegations
        </button>

        {open && (
          // Two buttons, not a `<select>`: a select is sized by its longest option
          // and cannot be styled or truncated, which is what pushed the composer off
          // the right edge of a phone.
          <div role="group" aria-label="Order delegations by" className="flex items-center gap-0.5">
            <OrderButton
              current={order}
              value="structure"
              label="structure"
              onSelect={setOrder}
            />
            <OrderButton
              current={order}
              value="duration"
              label="slowest first"
              onSelect={setOrder}
            />
          </div>
        )}

        {/* The caveat belongs beside the axis, not in a tooltip: a reader who cannot
            see which bars are faded has no way to ask for it. The two are counted
            apart because "no figure" and "a weak figure" send a reader to different
            places — the first looks like a bug, the second is read off the `≥`. */}
        {open && waterfall.bounded > 0 && (
          <p className="text-3xs text-ink-muted">
            {waterfall.bounded === count ? "Every" : `${waterfall.bounded} of ${count}`}{" "}
            {waterfall.bounded === 1 ? "row is" : "rows are"} a lower bound (
            <span aria-hidden="true">≥</span>
            <span className="sr-only">at least</span>) — a settled turn keeps each
            delegation&apos;s closing time, so its span is measured from the work it
            forwarded.
          </p>
        )}

        {open && waterfall.untimed > 0 && (
          <p className="text-3xs text-ink-muted">
            {waterfall.untimed} of {count} cannot be timed at all: nothing was forwarded
            to measure from.
          </p>
        )}
      </div>

      {open && (
        <>
          <ol className="space-y-0.5 px-1.5 pb-1">
            {waterfall.rows.map((row) => (
              <OverviewRow key={row.key} row={row} onFocus={onFocus} />
            ))}
          </ol>
          <AxisFooter spanMs={waterfall.spanMs} timed={waterfall.timed} />
        </>
      )}
    </section>
  );
}

function OrderButton({
  current,
  value,
  label,
  onSelect,
}: {
  current: WaterfallOrder;
  value: WaterfallOrder;
  label: string;
  onSelect: (order: WaterfallOrder) => void;
}) {
  const active = current === value;
  return (
    <button
      type="button"
      onClick={() => onSelect(value)}
      // `aria-pressed` rather than a visual highlight alone: which ordering is in
      // effect changes what the list means, so it cannot be conveyed by colour only.
      aria-pressed={active}
      className={`tap-row rounded px-1 text-3xs focus-ring ${
        active ? "bg-hover font-medium text-ink" : "text-ink-muted hover:bg-hover hover:text-ink-soft"
      }`}
    >
      {label}
    </button>
  );
}

/**
 * One trajectory: a label line, then its bar on the shared axis.
 *
 * Two lines rather than a table row. A table's columns are sized by their content, so
 * a long agent name would either force horizontal scrolling at 412px or squeeze the
 * axis to nothing — and the axis is the whole point. Stacked, the name truncates and
 * the bar always gets the full width.
 */
function OverviewRow({
  row,
  onFocus,
}: {
  row: WaterfallRow;
  onFocus: (trajectory: string) => void;
}) {
  // Shared with the card rather than recomputed. These are the two surfaces that name
  // one node, and a second copy of the rule lets them disagree — at which point this
  // row's `aria-label` no longer matches the region name the card renders, and the
  // locator that finds one stops finding the other.
  const name = trajectoryName(row.node);
  const indent = Math.min(row.depth, MAX_LABEL_INDENT);

  const body = (
    <>
      <span className="min-w-0 truncate">{name}</span>
      {row.depth > MAX_LABEL_INDENT && (
        <span className="shrink-0 rounded bg-hover px-1 text-3xs text-ink-muted">
          depth {row.depth}
        </span>
      )}
      {row.slowest && (
        <span className="shrink-0 rounded bg-hover px-1 text-3xs font-medium text-ink-soft">
          slowest
        </span>
      )}
      <span className="ml-auto shrink-0 text-3xs text-ink-muted">{timing(row)}</span>
    </>
  );

  return (
    <li>
      {/* Indentation is on the label alone. The track below is a sibling at full
          width, so every bar shares one origin whatever its depth — an indented
          track would offset a bar by its nesting and destroy the only comparison
          this view makes. Earlier this was one indented row with a negative margin
          on the track to cancel it, which is the same alignment expressed as two
          numbers that have to stay in sync. */}
      <div style={{ paddingLeft: `${indent * 0.75}rem` }}>
        {row.navigable ? (
          <button
            type="button"
            onClick={() => onFocus(row.node.trajectory!)}
            // The whole comparison, in words. A reader who never sees a bar gets the
            // same facts in the same order, which is what makes this an overview
            // rather than a picture with a caption.
            aria-label={describe(row, name)}
            className="tap-row flex w-full items-baseline gap-1.5 rounded px-1 text-left text-2xs text-ink hover:bg-hover focus-ring"
          >
            {body}
          </button>
        ) : (
          // Nothing to drill into, so not a control. A button that re-roots onto an
          // empty pane is the same broken promise as `aria-expanded` over nothing.
          //
          // The sentence cannot ride on `aria-label` here: naming is *prohibited* on a
          // paragraph, so axe reports it and a screen reader is entitled to ignore it.
          // The visible run is hidden and the same sentence supplied as text instead,
          // which is the only shape that carries one name for a non-widget.
          <div className="flex w-full items-baseline gap-1.5 px-1 text-2xs text-ink-muted">
            <span
              aria-hidden="true"
              className="flex min-w-0 flex-1 items-baseline gap-1.5"
            >
              {body}
            </span>
            <span className="sr-only">{describe(row, name)}</span>
          </div>
        )}
      </div>

      {/* The bar restates the label, so it carries no accessible content of its own.
          No `ring` anywhere in here: a ring is a box-shadow, and the focusable
          element is the label above, which uses `focus-ring` (an outline). */}
      <span
        aria-hidden="true"
        className="relative mx-1 block h-1 rounded-sm bg-hover"
      >
        {row.extent.kind === "instant" ? (
          // A caret, not a bar. There is no width to draw: the start was consolidated
          // away, and a bar here would be a fabricated measurement.
          <span
            className="absolute top-[-1px] h-[6px] w-[2px] rounded-sm bg-ink-faint"
            style={{ left: `${row.offset * 100}%` }}
          />
        ) : (
          <span
            className={`absolute inset-y-0 rounded-sm ${BAR_TONE[row.status]} ${
              // A bar whose end is only a bound has no real right edge. Fading it says
              // "continues" without claiming a number the data does not support.
              row.extent.kind === "open" ||
              (row.extent.kind === "measured" && row.extent.bound !== "exact")
                ? "opacity-70 [mask-image:linear-gradient(to_right,black_70%,transparent)]"
                : ""
            }`}
            style={{
              left: `${row.offset * 100}%`,
              width: `${Math.max(row.length * 100, 0.8)}%`,
            }}
          />
        )}
      </span>
    </li>
  );
}

/**
 * The axis, as two labels.
 *
 * No tick marks and no gridlines: the comparison here is between bars, not between a
 * bar and an absolute time, and ticks on a 412px axis are four unreadable numbers.
 * The turn's start and its full span are what make a bar's length mean something.
 */
function AxisFooter({ spanMs, timed }: { spanMs: number; timed: boolean }) {
  if (!timed) {
    return (
      <p className="px-2.5 pb-1 text-3xs text-ink-muted">
        No delegation in this turn reports both a start and an end, so there is no time
        axis to compare against.
      </p>
    );
  }
  return (
    <div className="flex items-baseline justify-between px-2.5 pb-1 text-3xs text-ink-muted">
      <span>turn start</span>
      <span>{formatDuration(spanMs)}</span>
    </div>
  );
}

/** The trailing figure on a row: elapsed time, or why there is none. */
function timing(row: WaterfallRow): string {
  if (row.extent.kind === "open") return "running";
  if (row.extent.kind === "instant") return "untimed";
  if (row.durationMs === undefined) return "";
  // Only an `exact` bound is a number; the other two are lower bounds and carry the
  // sign that says so, because a figure presented bare is read as a measurement.
  const prefix = row.extent.bound === "exact" ? "" : "≥";
  return `${prefix}${formatDuration(row.durationMs)}`;
}

/**
 * A row as one sentence, for the accessible name.
 *
 * Order matters: identity, then outcome, then cost, then position. That is the order
 * the eye takes the row in, and a screen-reader user scanning twenty of these needs
 * to be able to stop listening after the first two.
 */
function describe(row: WaterfallRow, name: string): string {
  const parts = [name, STATUS_WORD[row.status]];

  if (row.extent.kind === "measured") {
    const elapsed = formatDuration(row.durationMs ?? 0);
    // Each bound needs its own words. "at least 2s" and "2s" are different claims,
    // and saying the stronger one where only the weaker holds is the failure mode this
    // whole module is arranged around.
    if (row.extent.bound === "exact") parts.push(`took ${elapsed}`);
    else if (row.extent.bound === "stopped-listening") {
      parts.push(`ran at least ${elapsed} before its turn ended`);
    } else parts.push(`ran at least ${elapsed}, measured from the work it forwarded`);
  } else if (row.extent.kind === "instant") {
    parts.push("duration unknown, only its closing time was recorded");
  }

  if (row.slowest) parts.push("the longest here");

  if (row.startOffsetMs !== undefined && row.startOffsetMs > 0) {
    parts.push(`started ${formatDuration(row.startOffsetMs)} in`);
  } else if (row.startOffsetMs === 0) {
    parts.push("started with the turn");
  }

  if (row.depth > 0) parts.push(`nested ${row.depth} deep`);
  if (row.note === "no-interior") parts.push("forwarded none of its own events");
  // "Nothing is known" was the wrong tense: nobody has reported an outcome *yet*, and
  // a late one is written against the turn that owns the trajectory — so this row can
  // still acquire one. Only a restart makes it terminal.
  if (row.note === "unreported") parts.push("outlived its turn, no outcome reported yet");
  if (row.navigable) parts.push("activate to focus");

  // The delegate's own account, last. It is the longest part and the least useful for
  // *comparing*, which is what this list is for — a reader scanning twenty rows needs
  // to be able to stop listening before reaching a sentence. It is here at all because
  // a row that reports only a status and a duration says work happened and nothing
  // about what came of it, and this is not visible on the row: adding it there would
  // squeeze out the duration at 412px, where the name already truncates.
  const report = trajectoryReport(row.node);
  if (report) parts.push(report);

  return parts.join(", ");
}

/**
 * Duration, at the precision a reader can act on.
 *
 * Kept local rather than imported from the timeline: this one has to render a *span*
 * of an axis as well as an elapsed time, and 0ms is a legitimate value here where in
 * the timeline it would mean a missing measurement.
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  return `${minutes}m ${Math.round((ms % 60_000) / 1000)}s`;
}
