/**
 * Sibling comparison for a delegation fan-out: which of these agents was slow.
 *
 * # The question the tree could not answer
 *
 * `lib/timeline-tree.ts` makes a turn navigable, and each card's `Rollup` answers
 * *"should I open this branch"*. Neither answers *"which of these twenty was
 * expensive"*, because a rollup describes one node against itself — nothing compares
 * it to the node beside it. A user with a wide fan-out had to open every branch and
 * read twenty durations, which is exactly the work collapsing was supposed to save.
 *
 * That is the problem distributed tracing already solved, and this codebase already
 * borrowed `span` from it. One shared time axis, one bar per trajectory: a
 * concurrent fan-out reads as bars stacked in the same horizontal band, a sequential
 * chain as a staircase, and the slow one is the long bar. No chart library — the
 * layout is two numbers per row against a shared origin, so it is a pure function
 * and pins in unit tests rather than in a screenshot.
 *
 * # What the events genuinely carry, and what they do not
 *
 * Every event has `createdAtMs`, so a *live* trajectory is measurable: its opening
 * event is the start and `TimelineRow.closedAtMs` — or a promoted host's
 * `tool_result` — is the end. Three cases are not measurable, and each is reported
 * as itself rather than drawn as a bar that implies a measurement:
 *
 * 1. **Running.** No end has arrived. Deriving one from `Date.now()` would re-render
 *    every bar at clock rate inside a live stream, and the axis is taken from the
 *    data for exactly that reason; an open bar says "still going" instead.
 * 2. **Abandoned.** The server closed it when the turn terminated, so the close
 *    timestamp is when *we stopped listening*, not when the work ended. That is a
 *    lower bound, and it is labelled as one.
 * 3. **Consolidated.** `history::consolidate` folds a block's phases into one row
 *    keyed by the closing event, and `block_to_event` publishes only its
 *    `created_at_ms` — so after a reload a *declared* trajectory's `started` phase is
 *    gone and its start is genuinely unknown. `buildRows` then leaves `closedAtMs`
 *    unset, which is the signal. Such a row is an instant on the axis, not a bar:
 *    inventing a width there would be a fabricated measurement, and it is the case
 *    that occurs on every reloaded turn rather than a corner. A *promoted* host
 *    survives, because its call and its result are separate blocks with separate
 *    timestamps.
 *
 * The same honesty rule the cards follow applies to width: a `Workflow` whose
 * interior the vendor never forwarded has no measurable interior, so the row carries
 * a note saying so rather than an empty bar that reads as "did nothing".
 */

import { flattenTrajectories, type TimelineNode, type TrajectoryStatus } from "./timeline-tree";

/**
 * How much of a trajectory's extent is actually known.
 *
 * Split as a discriminated union rather than a nullable pair, because the renderer
 * must say something *different* in each case and a `start?: number` invites reading
 * a missing start as zero — which on a shared axis is not a gap, it is a claim that
 * the work began with the turn.
 */
export type Extent =
  | {
      kind: "measured";
      startMs: number;
      endMs: number;
      /** How far the two ends can be trusted. See `ExtentBound`. */
      bound: ExtentBound;
    }
  /** Started, still going. No end exists to draw to. */
  | { kind: "open"; startMs: number }
  /**
   * Nothing but the closing instant. A reloaded declared trajectory that forwarded
   * no interior: its phases were consolidated into one row, and it has no descendants
   * to bound it from either.
   */
  | { kind: "instant"; atMs: number };

/**
 * What a measured extent is a measurement *of*.
 *
 * Three different claims, and collapsing them into one boolean made the label state
 * the wrong one. Each has its own reason for being a bound rather than a fact, and a
 * reader deciding whether a number is worth acting on needs to know which.
 */
export type ExtentBound =
  /** Both ends are the trajectory's own. The number is what it says. */
  | "exact"
  /**
   * The end is when the turn terminated and we stopped listening, not when the work
   * stopped. A server-closed trajectory — and one that may yet report a real outcome
   * on a later turn, at which point the bound is replaced by an exact span rather than
   * refined. The layout is a pure function of the tree for that reason: nothing here
   * may hold a bound past the arrival of the fact that supersedes it.
   */
  | "stopped-listening"
  /**
   * Both ends come from what the trajectory *forwarded*, because its own start was
   * consolidated away. A lower bound: the work began no later than its first event
   * and ended no earlier than its last.
   */
  | "from-children";

/**
 * What a row must disclose about its *interior*, so the overview is not lossier than
 * the cards it summarizes.
 *
 * The two are mutually exclusive because they answer one question — what is behind
 * this row — and stating both says two things that cannot both be the point. Whether
 * the row could be *timed* is a different question, answered by `Extent`: a reloaded
 * hollow trajectory is legitimately both untimed and interior-less, and folding that
 * into one field forced a choice between two true statements.
 */
export type WaterfallNote =
  /** Finished and forwarded none of its interior. Real terminal state, not pending. */
  | "no-interior"
  /**
   * Still running when its turn ended, so no outcome has been reported yet.
   *
   * Not "nothing can be known": a late outcome is written against the turn that owns
   * the trajectory, so `abandoned` is not final and this row can be superseded. A
   * layout that treated it as terminal — caching it, or refusing to recompute — would
   * pin the unresolved state past the point where it stopped being true.
   */
  | "unreported";

/** One trajectory, placed on the shared axis. */
export interface WaterfallRow {
  node: TimelineNode;
  /** Trajectory ancestors above this one. Structure, not indentation budget. */
  depth: number;
  extent: Extent;
  /**
   * Bar geometry as fractions of the axis, `0` to `1`. The renderer turns these into
   * percentages, which is what makes the bar responsive without measuring the DOM —
   * and what keeps this module free of a layout pass.
   */
  offset: number;
  length: number;
  /** Elapsed time, when it is knowable. Absent for `open` and `instant`. */
  durationMs?: number;
  /** Time between the axis origin and this row's start, when it has one. */
  startOffsetMs?: number;
  /** The longest measured row in scope: the answer to "which one was slow". */
  slowest: boolean;
  note?: WaterfallNote;
  status: TrajectoryStatus;
  /** Whether drilling into this row can show anything. Mirrors the card's rule. */
  navigable: boolean;
  key: string;
}

/**
 * How the rows are ordered.
 *
 * `structure` is depth-first, which is the tree's own order and the one where a
 * staircase reads as a chain and a stack reads as a fan-out. `duration` throws the
 * hierarchy away and sorts longest first, because when the question is "which was
 * slow" a twenty-row scan for the widest bar is the work being avoided — and on a
 * 412px viewport, where twenty bars overlap in one band, it is not reliably doable
 * by eye at all.
 */
export type WaterfallOrder = "structure" | "duration";

export interface Waterfall {
  rows: WaterfallRow[];
  /** Axis origin: the earliest instant any row in scope reports. */
  originMs: number;
  /** Axis span. Zero when every row lands on one instant. */
  spanMs: number;
  /** Whether any row has a real duration. When false there is no axis to draw. */
  timed: boolean;
  /** Rows with no duration at all, for a one-line caveat. */
  untimed: number;
  /**
   * Rows whose duration is only a lower bound, counted separately.
   *
   * A caveat that lumped these together with `untimed` would say a figure is missing
   * where one is present but weak, which sends a reader looking for a bug instead of
   * reading the `≥`.
   */
  bounded: number;
}

/**
 * The narrowest bar that is still a bar.
 *
 * A 3ms trajectory beside a 40s one rounds to a zero-width box that reads as absent,
 * and "absent" is a different claim from "fast". Floored rather than log-scaled: a
 * log axis makes a 100× difference look like 2×, which defeats the one comparison
 * this view exists for.
 */
const MIN_LENGTH = 0.012;

/**
 * Place every trajectory in scope on one axis.
 *
 * `nodes` is whatever the timeline is currently showing — the turn's roots, or one
 * trajectory's children under focus — so the comparison is always between rows the
 * user can see as siblings. Scoping it to the focused subtree is what keeps a
 * hundred-trajectory workflow's overview about the branch in hand.
 */
export function buildWaterfall(
  nodes: TimelineNode[],
  order: WaterfallOrder = "structure",
): Waterfall {
  const entries = flattenTrajectories(nodes);

  const rows: WaterfallRow[] = entries.map(({ node, depth }) => {
    const extent = extentOf(node);
    return {
      node,
      depth,
      extent,
      offset: 0,
      length: 0,
      durationMs: measuredDuration(extent),
      slowest: false,
      note: noteFor(node),
      status: node.status ?? "running",
      navigable: navigable(node),
      key: node.key,
    };
  });

  // The axis is derived from the data and never from a clock, so it only moves when
  // an event moves it. A `Date.now()` end would re-lay out every bar on every frame
  // for the whole time a delegation runs.
  const instants: number[] = [];
  for (const row of rows) {
    if (row.extent.kind === "measured") instants.push(row.extent.startMs, row.extent.endMs);
    else if (row.extent.kind === "open") instants.push(row.extent.startMs);
    else instants.push(row.extent.atMs);
  }

  const originMs = instants.length ? Math.min(...instants) : 0;
  const endMs = instants.length ? Math.max(...instants) : 0;
  const spanMs = Math.max(0, endMs - originMs);

  const comparable = rows.filter((row) => row.durationMs !== undefined);

  // "Slowest" is a claim about rank, and a rank over lower bounds is not one.
  //
  // A row bounded `from-children` is measured from its first forwarded event, which
  // can be arbitrarily late: a delegate that ran 2s and spoke once at 1.9s bounds at
  // 0.1s. Ranking that against an exact 0.5s would name the wrong row and label the
  // mistake "slowest" — a confident answer to the one question this view exists for,
  // derived from data that cannot support it. So the badge requires that every row in
  // the comparison is exact; when any is not, the durations are still shown, with
  // their `≥`, and no row claims the top.
  // Two conditions, because the extent and the status can disagree and each one alone
  // lets a bad rank through.
  //
  // `bound === "exact"` says the *arithmetic* is sound. It does not say the work
  // finished: a trajectory still open when its turn ended can acquire an end
  // timestamp — the close the server wrote, or the turn's own end — and that produces
  // an exact-looking subtraction over a span whose real end nobody reported. Such a
  // row is labelled "never finished" and was still eligible to be called "the longest
  // here", which is the one sentence this view must never get wrong. Its true duration
  // can only be longer, so it cannot lose a comparison and must not win one.
  //
  // Publishing `startedAtMs` is what surfaced this: while every settled row was a
  // bound, no rank was ever offered, so the gap could not be observed. Making the
  // honest rows exact is what let a dishonest one be crowned.
  const finished = (row: WaterfallRow) => row.status === "done" || row.status === "failed";
  const rankableRows = rows.filter(
    (row) => row.extent.kind === "measured" && row.extent.bound === "exact" && finished(row),
  );
  const rankable = rankableRows.length === comparable.length && comparable.length > 1;
  if (rankable) {
    let longest = -1;
    for (const row of comparable) {
      if (row.durationMs! > longest) longest = row.durationMs!;
    }
    // Only when something is actually longer than something else. Marking the single
    // row of a one-delegate turn "slowest" states a comparison that was never made.
    if (longest > 0) {
      // Over `rankableRows`, not every row: a row excluded from the comparison must
      // not be able to collect the badge by matching the winning figure.
      for (const row of rankableRows) if (row.durationMs === longest) row.slowest = true;
    }
  }

  for (const row of rows) {
    const { extent } = row;
    if (extent.kind === "instant") {
      // A caret, not a bar: the width would be a fabricated measurement.
      row.offset = fraction(extent.atMs - originMs, spanMs);
      row.length = 0;
      continue;
    }
    row.startOffsetMs = extent.startMs - originMs;
    row.offset = fraction(row.startOffsetMs, spanMs);
    if (extent.kind === "open") {
      // Runs to the end of what is known, which is the honest extent of an
      // unfinished span — it is at least this long.
      row.length = Math.max(MIN_LENGTH, 1 - row.offset);
      continue;
    }
    const raw = fraction(extent.endMs - extent.startMs, spanMs);
    row.length = Math.min(1 - row.offset, Math.max(MIN_LENGTH, raw));
  }

  if (order === "duration") {
    // Untimed rows sink rather than sort as zero: zero is a duration, and reading
    // "this took no time" off a row whose start was consolidated away is precisely
    // the fabricated measurement the `instant` case exists to avoid.
    rows.sort((a, b) => rank(b) - rank(a));
  }

  return {
    rows,
    originMs,
    spanMs,
    timed: comparable.length > 0,
    untimed: rows.length - comparable.length,
    bounded: rows.filter(
      (row) => row.extent.kind === "measured" && row.extent.bound !== "exact",
    ).length,
  };
}

/** Sort key for duration order. Negative for a row with no duration. */
function rank(row: WaterfallRow): number {
  return row.durationMs ?? -1;
}

function fraction(value: number, span: number): number {
  if (span <= 0) return 0;
  return Math.min(1, Math.max(0, value / span));
}

function measuredDuration(extent: Extent): number | undefined {
  if (extent.kind !== "measured") return undefined;
  return Math.max(0, extent.endMs - extent.startMs);
}

/**
 * What is known about when a trajectory ran.
 *
 * The two kinds of trajectory answer this differently, which is the same split every
 * other rule about promotion has to make. A declared `subagent`'s row is folded
 * across its phases, so its `createdAtMs` is the `started` phase and `closedAtMs` is
 * the close. A promoted `tool_call`'s row keeps the *call's* timestamp, and its close
 * is the `tool_result` sharing its id — reading its phase instead would time the
 * argument streaming rather than the delegated work
 * (`docs/session-protocol.md` § Promotion).
 */
function extentOf(node: TimelineNode): Extent {
  const { row, status } = node;
  const startMs = row.event.createdAtMs;
  const promoted = row.event.kind !== "subagent";

  // A promoted host's own result is its close, and it survives consolidation because
  // the call and the result are separate blocks with separate timestamps.
  const endMs = row.closedAtMs ?? (promoted ? row.result?.createdAtMs : undefined);

  if (endMs !== undefined) {
    return {
      kind: "measured",
      startMs,
      // Clamped: a clock adjustment mid-turn would otherwise produce a negative
      // width, which lays out as a bar running backwards off the left edge.
      endMs: Math.max(startMs, endMs),
      bound: status === "abandoned" ? "stopped-listening" : "exact",
    };
  }

  if (status === "running") return { kind: "open", startMs };

  // Closed with no close timestamp: a *declared* trajectory whose phases were
  // consolidated into one row, so `createdAtMs` is its close.
  //
  // `startedAtMs` is the block's own record of when its first phase arrived, and it
  // makes this exact rather than reconstructed. It is present only on a consolidated
  // block — a live event is one arrival and says nothing about a span — so its
  // presence is precisely the case being handled here.
  //
  // This matters more than one field sounds like: a turn switches to consolidated
  // history moments after it settles, so *every* delegation a user scrolls back to is
  // in this state. While the field went unpublished, every such duration was a lower
  // bound, and a lower bound cannot be ranked — so the "slowest" answer this view
  // exists to give was withheld on all but a live turn.
  if (!promoted) {
    const declaredStart = row.event.startedAtMs;
    if (declaredStart !== undefined && declaredStart < startMs) {
      return { kind: "measured", startMs: declaredStart, endMs: startMs, bound: "exact" };
    }

    // No `startedAtMs`: history written before the field was published, or a block
    // whose phases all arrived in one millisecond. The interior still bounds it from
    // below, since every descendant is its own timestamped block. Labelled as the
    // bound it is.
    const first = earliestDescendant(node);
    if (first !== undefined && first < startMs) {
      return { kind: "measured", startMs: first, endMs: startMs, bound: "from-children" };
    }
    return { kind: "instant", atMs: startMs };
  }

  // A promoted host with neither a result nor a close. Nothing said when it ended.
  return { kind: "open", startMs };
}

/**
 * The earliest timestamp anywhere beneath a node.
 *
 * Iterative with a visited set, for the reason every walk in `timeline-tree.ts`
 * carries one: `buildTree` repairs a `span` cycle by reachability, so the tree it
 * returns can be reachable and still cyclic.
 */
function earliestDescendant(node: TimelineNode): number | undefined {
  let earliest: number | undefined;
  const seen = new Set<TimelineNode>([node]);
  const stack = [...node.children];
  while (stack.length) {
    const next = stack.pop()!;
    if (seen.has(next)) continue;
    seen.add(next);
    const at = next.row.event.createdAtMs;
    if (earliest === undefined || at < earliest) earliest = at;
    stack.push(...next.children);
  }
  return earliest;
}

/**
 * What the row must disclose besides its bar.
 *
 * Deliberately the same three statements `TrajectoryCard` makes, because an overview
 * that omitted them would be lossier than the cards it summarizes — and the whole
 * defence of collapsing is that nothing hidden goes unreported.
 */
function noteFor(node: TimelineNode): WaterfallNote | undefined {
  if (node.children.length > 0) return undefined;
  if (node.status === "abandoned") return "unreported";
  if (node.status === "running") return undefined;
  return "no-interior";
}

/**
 * Whether focusing this row can show anything.
 *
 * Mirrors `TrajectoryCard`'s `openable`: a promoted host always has its own call to
 * show, and a declared trajectory with no children has nothing at all. Offering a
 * control that re-roots onto an empty pane is the same broken promise as
 * `aria-expanded` over nothing.
 */
function navigable(node: TimelineNode): boolean {
  if (node.row.event.kind !== "subagent") return true;
  return node.children.length > 0;
}
