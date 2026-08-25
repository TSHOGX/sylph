import { describe, expect, it } from "vitest";
import { buildRows, buildTree } from "@/lib/timeline-tree";
import { buildWaterfall, type WaterfallRow } from "@/lib/timeline-waterfall";
import type { StreamEvent } from "@/lib/api";

/**
 * Sibling comparison over one time axis.
 *
 * Every failure this pins is a *plausible* number. A bar laid out from a start that
 * was consolidated away reads as a real measurement; an abandoned trajectory drawn
 * as a finished one reports an interrupted turn as a completed one; and a fan-out
 * whose bars all begin at the origin says the agents ran in sequence when they ran at
 * once. None of those look wrong on screen, which is why they are asserted on the
 * layout rather than on the rendering.
 */

let seq = 0;
function ev(partial: Partial<StreamEvent> & { kind: string }): StreamEvent {
  return {
    seq: (seq += 1),
    stability: "stable",
    source: "agent",
    createdAtMs: 0,
    ...partial,
  };
}

function opens(id: string, at: number, name?: string, span?: string): StreamEvent {
  return ev({
    kind: "subagent",
    phase: "started",
    streamId: id,
    name,
    span,
    createdAtMs: at,
  });
}

function closes(
  id: string,
  at: number,
  extra: Partial<StreamEvent> = {},
): StreamEvent {
  return ev({
    kind: "subagent",
    phase: "completed",
    streamId: id,
    createdAtMs: at,
    ...extra,
  });
}

/** A trajectory the server closed on turn termination. */
function serverCloses(id: string, at: number): StreamEvent {
  return closes(id, at, { isError: true, data: { closedBy: "server" } });
}

function waterfall(events: StreamEvent[], order?: "structure" | "duration") {
  return buildWaterfall(buildTree(buildRows(events)), order);
}

function named(rows: WaterfallRow[]): string[] {
  return rows.map((row) => row.node.row.event.name ?? "?");
}

describe("buildWaterfall", () => {
  it("places concurrent delegates in the same band and a chain as a staircase", () => {
    // The one visual distinction the view exists for: both shapes occur, and on a
    // rollup they are indistinguishable because a rollup never compares siblings.
    const fanout = waterfall([
      opens("a", 1000, "First"),
      opens("b", 1000, "Second"),
      closes("a", 3000),
      closes("b", 3000),
    ]);

    expect(fanout.rows.map((row) => row.offset)).toEqual([0, 0]);
    expect(fanout.rows.map((row) => row.length)).toEqual([1, 1]);

    const chain = waterfall([
      opens("a", 1000, "First"),
      closes("a", 2000),
      opens("b", 2000, "Second"),
      closes("b", 3000),
    ]);

    expect(chain.rows.map((row) => row.offset)).toEqual([0, 0.5]);
    expect(chain.rows.map((row) => row.length)).toEqual([0.5, 0.5]);
  });

  it("names the slowest sibling, and only when there is a comparison to make", () => {
    const wide = waterfall([
      opens("a", 0, "Quick"),
      opens("b", 0, "Slow"),
      closes("a", 100),
      closes("b", 9000),
    ]);

    expect(wide.rows.filter((row) => row.slowest).map((row) => row.node.row.event.name)).toEqual(
      ["Slow"],
    );

    // One delegate compares to nothing, so calling it slowest states a comparison
    // nobody made.
    const single = waterfall([opens("a", 0, "Only"), closes("a", 5000)]);
    expect(single.rows.some((row) => row.slowest)).toBe(false);
  });

  it("draws a running delegate open-ended rather than inventing an end", () => {
    // A `Date.now()` end would re-lay out every bar at clock rate for the whole time
    // a delegation runs, and would report a duration that grows while nothing happens.
    const { rows, timed } = waterfall([
      opens("a", 1000, "Done"),
      closes("a", 2000),
      opens("b", 1500, "Running"),
    ]);

    const running = rows[1];
    expect(running.extent.kind).toBe("open");
    expect(running.durationMs).toBeUndefined();
    // Runs to the end of what is known: it is at least this long.
    expect(running.offset).toBeCloseTo(0.5, 5);
    expect(running.length).toBeCloseTo(0.5, 5);
    expect(timed).toBe(true);
  });

  it("marks an abandoned trajectory's end as a lower bound, not a finish", () => {
    // The server's close records when the turn stopped, not when the work did.
    // Presenting it as a finished span reports an interrupted turn as a completed one.
    const { rows } = waterfall([opens("a", 0, "Stranded"), serverCloses("a", 4000)]);

    expect(rows[0].status).toBe("abandoned");
    expect(rows[0].extent).toMatchObject({
      kind: "measured",
      bound: "stopped-listening",
    });
    expect(rows[0].durationMs).toBe(4000);
  });

  it("refuses to give a consolidated trajectory a width", () => {
    // What a reloaded turn looks like: `history::consolidate` folds the phases into
    // one row keyed by the close, and `block_to_event` publishes only that timestamp.
    // The start is genuinely gone, so any bar drawn here is a fabricated measurement —
    // and this is every reloaded turn, not a corner case.
    const { rows, timed, untimed } = waterfall([
      closes("a", 5000, { name: "Reloaded" }),
    ]);

    expect(rows[0].extent).toEqual({ kind: "instant", atMs: 5000 });
    expect(rows[0].length).toBe(0);
    expect(rows[0].durationMs).toBeUndefined();
    expect(timed).toBe(false);
    expect(untimed).toBe(1);
  });

  it("bounds a consolidated trajectory by the work it forwarded", () => {
    // This is what makes the view usable at all on a settled turn, which is every turn
    // a user scrolls back to: the transcript switches to consolidated history moments
    // after settling. The trajectory's own start is gone, but each descendant is still
    // its own block with its own timestamp, so the interior bounds it from below.
    const { rows, timed, bounded, untimed } = waterfall([
      closes("s1", 5000, { name: "Reloaded" }),
      ev({
        kind: "assistant",
        phase: "completed",
        streamId: "b1",
        span: "s1",
        text: "work",
        createdAtMs: 4000,
      }),
    ]);

    expect(rows[0].extent).toEqual({
      kind: "measured",
      startMs: 4000,
      endMs: 5000,
      bound: "from-children",
    });
    expect(rows[0].durationMs).toBe(1000);
    expect(timed).toBe(true);
    // Counted as a bound, not as a measurement and not as missing: "no figure" and "a
    // weak figure" send a reader to different places.
    expect(bounded).toBe(1);
    expect(untimed).toBe(0);
  });

  it("prefers a consolidated block's own start over a bound from its children", () => {
    // `startedAtMs` is the block's record of when its first phase arrived, published
    // only on a consolidated block. With it, a settled trajectory is exact rather than
    // bounded — which is the difference between being rankable and not, since a rank
    // over lower bounds is not a rank.
    //
    // The same shape as the test above, plus the one field. Note the descendant at
    // 4000 would have produced a `from-children` bound of 1000ms; the block's own
    // start is earlier and truer, because the delegate existed before it forwarded
    // anything.
    const { rows, timed, bounded, untimed } = waterfall([
      closes("s1", 5000, { name: "Reloaded", startedAtMs: 3000 }),
      ev({
        kind: "assistant",
        phase: "completed",
        streamId: "b1",
        span: "s1",
        text: "work",
        createdAtMs: 4000,
      }),
    ]);

    expect(rows[0].extent).toEqual({
      kind: "measured",
      startMs: 3000,
      endMs: 5000,
      bound: "exact",
    });
    expect(rows[0].durationMs).toBe(2000);
    expect(timed).toBe(true);
    expect(bounded).toBe(0);
    expect(untimed).toBe(0);
  });

  it("never crowns a row that did not finish, even when its arithmetic is exact", () => {
    // A trajectory still open when its turn ended can acquire an end timestamp — the
    // close the server wrote, or the turn's own end — so the subtraction is exact over
    // a span whose real end nobody reported. Its true duration can only be longer, so
    // it cannot lose a comparison and must not win one. It was eligible for "the
    // longest here" while carrying the label "never finished".
    //
    // Only observable once settled rows became exact: while every one was a bound, no
    // rank was offered at all, so nothing could be wrongly crowned.
    const { rows } = waterfall([
      closes("fast", 2000, { name: "Fast", startedAtMs: 1000 }),
      closes("slow", 4000, { name: "Slow", startedAtMs: 1000 }),
      // Open at 1000, force-closed at 9000 by the turn ending. Reads as the longest.
      ev({
        kind: "subagent",
        phase: "started",
        streamId: "cut",
        name: "Cutoff",
        createdAtMs: 1000,
      }),
      serverCloses("cut", 9000),
    ]);

    const byName = (n: string) => rows.find((r) => r.node.row.event.name === n)!;
    expect(byName("Cutoff").status).toBe("abandoned");
    expect(byName("Cutoff").slowest).toBe(false);
    // And because one row is unrankable, no row claims the top at all.
    expect(rows.filter((r) => r.slowest)).toHaveLength(0);
  });

  it("refuses to name a slowest row when any duration is only a bound", () => {
    // A rank over lower bounds is not a rank. A delegate that ran 9s and forwarded its
    // one event at 8.9s bounds at 0.1s, so ranking it against an exact 0.5s names the
    // wrong row — and names it confidently, in answer to the only question this view
    // exists for. The figures are still shown; the badge is what is withheld.
    const { rows, bounded } = waterfall([
      opens("live", 0, "Exactly timed"),
      closes("live", 500),
      // Consolidated: no `started` phase, one late child.
      closes("folded", 9000, { name: "Bounded" }),
      ev({
        kind: "assistant",
        phase: "completed",
        streamId: "b1",
        span: "folded",
        text: "spoke late",
        createdAtMs: 8900,
      }),
    ]);

    expect(bounded).toBe(1);
    expect(rows.some((row) => row.slowest)).toBe(false);
    // The one that really ran longest is the one the bound understates, which is
    // exactly why no badge may be placed.
    expect(rows.map((row) => row.durationMs)).toEqual([500, 100]);
  });

  it("reports a bound and 'no interior' independently", () => {
    // A row can legitimately be both untimed and interior-less, and they answer
    // different questions: one is about duration, the other about what is behind the
    // row. Folding them into one field forced a choice between two true statements.
    const hollow = waterfall([closes("s2", 5000, { name: "Reloaded empty" })]);

    // Nothing was forwarded, so there is nothing to bound it from either.
    expect(hollow.rows[0].extent.kind).toBe("instant");
    expect(hollow.rows[0].note).toBe("no-interior");
  });

  it("prefers a trajectory's own span over its children's", () => {
    // A live trajectory reports both phases, and its own start precedes its first
    // child's. Reading the children there would report the delegate as having begun
    // when it first spoke, which is later than when it began.
    const { rows } = waterfall([
      opens("s1", 1000, "Live"),
      ev({
        kind: "assistant",
        phase: "completed",
        streamId: "b1",
        span: "s1",
        text: "work",
        createdAtMs: 2000,
      }),
      closes("s1", 4000),
    ]);

    expect(rows[0].extent).toEqual({
      kind: "measured",
      startMs: 1000,
      endMs: 4000,
      bound: "exact",
    });
  });

  it("times a promoted host from its call to its result", () => {
    // A promoted host's `completed` phase means its *arguments* finished streaming, so
    // reading the phase would time the argument parse rather than the delegated work.
    // Its call and result are separate blocks, which is why this survives a reload.
    const { rows } = waterfall([
      ev({
        kind: "tool_call",
        phase: "completed",
        streamId: "host",
        name: "Workflow",
        createdAtMs: 1000,
      }),
      opens("t1", 1100, "Demo: agent-1", "host"),
      closes("t1", 4000),
      ev({
        kind: "tool_result",
        phase: "completed",
        streamId: "host",
        name: "Workflow",
        createdAtMs: 5000,
      }),
    ]);

    const host = rows.find((row) => row.node.trajectory === "host")!;
    expect(host.extent).toMatchObject({ kind: "measured", startMs: 1000, endMs: 5000 });
    expect(host.durationMs).toBe(4000);
    // And the delegate is nested one level under it, on the same axis.
    const delegate = rows.find((row) => row.node.trajectory === "t1")!;
    expect(delegate.depth).toBe(1);
    expect(delegate.offset).toBeCloseTo(0.025, 5);
  });

  it("says a host forwarded nothing rather than drawing an empty interior", () => {
    // Same rule as the card: a `Workflow` whose interior the vendor never forwarded
    // has no measurable interior, and an empty bar claims it did nothing.
    const { rows } = waterfall([
      ev({
        kind: "tool_call",
        phase: "completed",
        streamId: "host",
        name: "Agent",
        createdAtMs: 0,
      }),
      closes("host", 2000, { text: "reported nothing" }),
    ]);

    expect(rows[0].note).toBe("no-interior");
    // Still navigable: a promoted host has its own call to show.
    expect(rows[0].navigable).toBe(true);
  });

  it("keeps a declared trajectory with nothing behind it out of reach", () => {
    // Offering a control that re-roots onto an empty pane is the same broken promise
    // as `aria-expanded` over nothing.
    const { rows } = waterfall([opens("a", 0, "Hollow"), closes("a", 10)]);

    expect(rows[0].note).toBe("no-interior");
    expect(rows[0].navigable).toBe(false);
  });

  it("distinguishes 'forwarded nothing' from 'nobody looked'", () => {
    const { rows } = waterfall([
      opens("a", 0, "Finished empty"),
      closes("a", 10),
      opens("b", 0, "Cut off"),
      serverCloses("b", 10),
    ]);

    expect(rows.map((row) => row.note)).toEqual(["no-interior", "unreported"]);
  });

  it("sorts longest first on request, and sinks what it cannot time", () => {
    // Twenty overlapping bars in a 412px band are not reliably comparable by eye,
    // which is the case this ordering exists for.
    const { rows } = waterfall(
      [
        opens("a", 0, "Middle"),
        closes("a", 500),
        opens("b", 0, "Longest"),
        closes("b", 9000),
        closes("c", 300, { name: "Untimed" }),
        opens("d", 0, "Shortest"),
        closes("d", 50),
      ],
      "duration",
    );

    expect(named(rows)).toEqual(["Longest", "Middle", "Shortest", "Untimed"]);
    // Zero is a duration. A row whose start was consolidated away has none, so it
    // must not sort as if it took no time.
    expect(rows[3].durationMs).toBeUndefined();
  });

  it("keeps depth-first order and nesting depth in structure mode", () => {
    const { rows } = waterfall([
      opens("s1", 0, "Workflow"),
      opens("s2", 10, "Phase", "s1"),
      opens("s3", 20, "Worker", "s2"),
      closes("s3", 30),
      closes("s2", 40),
      closes("s1", 50),
      opens("s4", 60, "After"),
      closes("s4", 70),
    ]);

    expect(named(rows)).toEqual(["Workflow", "Phase", "Worker", "After"]);
    expect(rows.map((row) => row.depth)).toEqual([0, 1, 2, 0]);
  });

  it("gives the shortest bar a visible width without log-scaling the axis", () => {
    // A 3ms trajectory beside a 40s one rounds to zero width, and absent is a
    // different claim from fast. A log scale would instead make a 10000x difference
    // look like 4x, which defeats the only comparison the view is for.
    const { rows } = waterfall([
      opens("a", 0, "Blink"),
      closes("a", 3),
      opens("b", 0, "Long"),
      closes("b", 40_000),
    ]);

    expect(rows[0].length).toBeGreaterThan(0);
    expect(rows[0].length).toBeLessThan(0.05);
    expect(rows[1].length).toBe(1);
  });

  it("survives a turn where every trajectory lands on one instant", () => {
    // The fixture shape, and every fast local agent: zero span must not divide by zero
    // or produce NaN geometry, which lays out as a bar of no width at no offset.
    const { rows, spanMs, timed } = waterfall([
      opens("a", 1000, "A"),
      closes("a", 1000),
      opens("b", 1000, "B"),
      closes("b", 1000),
    ]);

    expect(spanMs).toBe(0);
    expect(timed).toBe(true);
    for (const row of rows) {
      expect(Number.isFinite(row.offset)).toBe(true);
      expect(Number.isFinite(row.length)).toBe(true);
      expect(row.durationMs).toBe(0);
    }
    // Zero-length everywhere is not a comparison, so nothing is the slowest.
    expect(rows.some((row) => row.slowest)).toBe(false);
  });

  it("returns nothing for a turn that never delegated", () => {
    const { rows, timed } = waterfall([
      ev({ kind: "assistant", phase: "completed", streamId: "b1", text: "hello" }),
    ]);

    expect(rows).toEqual([]);
    expect(timed).toBe(false);
  });

  it("never clips a bar past the end of the axis", () => {
    // A clock adjustment mid-turn produces an end before the start, which lays out as
    // a bar running backwards off the left edge.
    const { rows } = waterfall([
      opens("a", 5000, "Skewed"),
      closes("a", 1000),
      opens("b", 1000, "Normal"),
      closes("b", 9000),
    ]);

    for (const row of rows) {
      expect(row.offset).toBeGreaterThanOrEqual(0);
      expect(row.offset + row.length).toBeLessThanOrEqual(1.000001);
    }
    expect(rows[0].durationMs).toBe(0);
  });

  it("terminates on a span cycle instead of freezing the tab", () => {
    // Two rows naming each other. `buildTree` repairs this by reachability, which
    // roots the survivor without breaking the child pointers — so the tree is
    // reachable and still cyclic, and an unguarded walk over `children` recurses
    // forever. Every other walk in the tree module already guarded; the outline one
    // did not, and this view iterates it on every build.
    const { rows } = waterfall([
      opens("a", 0, "First", "b"),
      opens("b", 10, "Second", "a"),
    ]);

    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThanOrEqual(2);
  });
});
