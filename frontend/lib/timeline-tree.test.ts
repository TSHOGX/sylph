import { describe, expect, it } from "vitest";
import {
  buildRows,
  buildTree,
  flattenTrajectories,
  pathTo,
  type TimelineNode,
} from "@/lib/timeline-tree";
import type { StreamEvent } from "@/lib/api";

/**
 * Assembling a turn's events into the tree of delegated trajectories it describes.
 *
 * The failure modes these pin are all silent. A child attached to the wrong parent
 * reads as a plausible tree that misdescribes what happened; a dropped orphan
 * loses work in exactly the case where structure is least reliable; and a rollup
 * that misses a descendant failure means a collapsed card shows green over a
 * broken delegate — which is worse than not collapsing at all.
 */
let seq = 0;
function ev(partial: Partial<StreamEvent> & { kind: string }): StreamEvent {
  return {
    seq: (seq += 1),
    stability: "stable",
    source: "agent",
    createdAtMs: seq,
    ...partial,
  };
}

/** `subagent` phases, as an adapter emits them. */
function opens(id: string, name?: string, span?: string): StreamEvent {
  return ev({ kind: "subagent", phase: "started", streamId: id, name, span });
}
function closes(id: string, span?: string, isError?: boolean): StreamEvent {
  return ev({ kind: "subagent", phase: "completed", streamId: id, span, isError });
}

function tree(events: StreamEvent[]): TimelineNode[] {
  return buildTree(buildRows(events));
}

/** Trajectory ids at each level, for compact structural assertions. */
function shape(nodes: TimelineNode[]): unknown[] {
  return nodes.map((node) =>
    node.children.length > 0
      ? [node.trajectory ?? node.row.event.kind, shape(node.children)]
      : (node.trajectory ?? node.row.event.kind),
  );
}

describe("buildTree", () => {
  it("nests an event under the trajectory it names", () => {
    const nodes = tree([
      opens("s1", "Explore"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "s1", text: "found it" }),
      closes("s1"),
    ]);

    expect(nodes).toHaveLength(1);
    expect(nodes[0].trajectory).toBe("s1");
    expect(nodes[0].children).toHaveLength(1);
    expect(nodes[0].children[0].row.event.text).toBe("found it");
  });

  it("nests a trajectory inside a trajectory, to arbitrary depth", () => {
    // The case a flat "subagent" list cannot express, and the reason `span` is a
    // pointer rather than a boolean.
    const nodes = tree([
      opens("s1", "Workflow"),
      opens("s2", "Phase", "s1"),
      opens("s3", "Worker", "s2"),
      ev({ kind: "tool_call", phase: "completed", streamId: "t1", span: "s3", name: "Bash" }),
      closes("s3", "s2"),
      closes("s2", "s1"),
      closes("s1"),
    ]);

    expect(shape(nodes)).toEqual([["s1", [["s2", [["s3", ["tool_call"]]]]]]]);
    expect(nodes[0].rollup.depth).toBe(3);
    expect(nodes[0].rollup.trajectories).toBe(2);
  });

  it("keeps concurrent siblings apart when their events interleave", () => {
    // Two delegates running at once emit onto one wire in arrival order. Grouping
    // has to come from `span`, because sequence tells you nothing about ownership.
    const nodes = tree([
      opens("a", "First"),
      opens("b", "Second"),
      ev({ kind: "assistant", phase: "completed", streamId: "x1", span: "a", text: "from a" }),
      ev({ kind: "assistant", phase: "completed", streamId: "x2", span: "b", text: "from b" }),
      ev({ kind: "assistant", phase: "completed", streamId: "x3", span: "a", text: "a again" }),
      closes("b"),
      closes("a"),
    ]);

    expect(nodes).toHaveLength(2);
    expect(nodes[0].children.map((c) => c.row.event.text)).toEqual(["from a", "a again"]);
    expect(nodes[1].children.map((c) => c.row.event.text)).toEqual(["from b"]);
  });

  it("holds a child that arrives before its parent, then attaches it", () => {
    // Out-of-order delivery. Rooting it immediately would show the delegate's work
    // at two depths once the parent appeared.
    const nodes = tree([
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "s1", text: "early" }),
      opens("s1", "Late"),
      closes("s1"),
    ]);

    expect(nodes).toHaveLength(1);
    expect(nodes[0].trajectory).toBe("s1");
    expect(nodes[0].children).toHaveLength(1);
  });

  it("shows an event whose trajectory never arrives, rather than dropping it", () => {
    // A link that died mid-delegation. Losing this hides the work the user is
    // looking for, so it surfaces at the root.
    const nodes = tree([
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "ghost", text: "orphan" }),
    ]);

    expect(nodes).toHaveLength(1);
    expect(nodes[0].row.event.text).toBe("orphan");
  });

  it("escalates a parent to failed when a descendant failed", () => {
    // A collapsed card is all the user sees, so a green row over a broken delegate
    // three levels down would make collapsing lossy.
    const nodes = tree([
      opens("s1", "Outer"),
      opens("s2", "Inner", "s1"),
      ev({
        kind: "tool_call",
        phase: "completed",
        streamId: "t1",
        span: "s2",
        name: "Bash",
      }),
      ev({
        kind: "tool_result",
        phase: "completed",
        streamId: "t1",
        span: "s2",
        isError: true,
        text: "exit 1",
      }),
      closes("s2", "s1"),
      closes("s1"),
    ]);

    expect(nodes[0].status).toBe("failed");
    expect(nodes[0].rollup.failures).toBe(1);
  });

  it("reports a trajectory the server closed as abandoned, not failed", () => {
    // Different questions: failed means the work reported an error, abandoned
    // means nobody ever said. An interrupted turn is not a broken one.
    const nodes = tree([
      opens("s1", "Cut short"),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "s1",
        isError: true,
        data: { closedBy: "server", reason: "interrupted" },
      }),
    ]);

    expect(nodes[0].status).toBe("abandoned");
  });

  it("reports a trajectory with no close as still running", () => {
    const nodes = tree([opens("s1", "Working")]);
    expect(nodes[0].status).toBe("running");
  });

  it("sums token usage across the whole subtree", () => {
    const nodes = tree([
      opens("s1"),
      ev({ kind: "usage", phase: "completed", streamId: "u1", span: "s1",
           data: { inputTokens: 100, outputTokens: 20 } }),
      opens("s2", undefined, "s1"),
      ev({ kind: "usage", phase: "completed", streamId: "u2", span: "s2",
           data: { input_tokens: 5, output_tokens: 1 } }),
      closes("s2", "s1"),
      closes("s1"),
    ]);

    expect(nodes[0].rollup.tokens).toEqual({ input: 105, output: 21 });
  });

  it("times a closed trajectory and leaves a running one untimed", () => {
    // A running duration would need a clock, and a clock re-renders the row
    // forever. `running` is the fact that matters while it is still true.
    const rows = buildRows([
      ev({ kind: "subagent", phase: "started", streamId: "s1", createdAtMs: 1000 }),
      ev({ kind: "subagent", phase: "completed", streamId: "s1", createdAtMs: 3500 }),
      ev({ kind: "subagent", phase: "started", streamId: "s2", createdAtMs: 1000 }),
    ]);
    const nodes = buildTree(rows);

    expect(nodes[0].rollup.durationMs).toBe(2500);
    expect(nodes[1].rollup.durationMs).toBeUndefined();
  });

  it("keeps the name a later phase supplied", () => {
    // The agent's type is only known once its streamed brief parses, so `started`
    // carries a placeholder. Replacing the row on `completed` — right for a text
    // block — would leave every card reading the placeholder.
    const nodes = tree([
      opens("s1", "Subagent"),
      ev({ kind: "subagent", phase: "delta", streamId: "s1", name: "code-reviewer" }),
      closes("s1"),
    ]);

    expect(nodes[0].row.event.name).toBe("code-reviewer");
    expect(nodes[0].status).toBe("done");
  });

  it("leaves a root-level stream untouched when nothing delegates", () => {
    // The common turn. A tree that reshapes a flat conversation would be a
    // regression for every session that never delegates.
    const nodes = tree([
      ev({ kind: "user", text: "go" }),
      ev({ kind: "assistant", phase: "completed", streamId: "a1", text: "done" }),
    ]);

    expect(nodes).toHaveLength(2);
    expect(nodes.every((n) => n.children.length === 0)).toBe(true);
    expect(nodes.every((n) => n.trajectory === undefined)).toBe(true);
  });
});

describe("pathTo", () => {
  it("returns the ancestry of a nested trajectory, for a breadcrumb", () => {
    const nodes = tree([
      opens("s1", "Outer"),
      opens("s2", "Inner", "s1"),
      closes("s2", "s1"),
      closes("s1"),
    ]);

    expect(pathTo(nodes, "s2")?.map((n) => n.trajectory)).toEqual(["s1", "s2"]);
  });

  it("answers null for a trajectory that is gone, so focus can fall back", () => {
    expect(pathTo(tree([opens("s1")]), "missing")).toBeNull();
  });
});

describe("flattenTrajectories", () => {
  it("lists every trajectory with its depth, skipping plain events", () => {
    const nodes = tree([
      opens("s1"),
      ev({ kind: "assistant", phase: "completed", streamId: "b", span: "s1", text: "x" }),
      opens("s2", undefined, "s1"),
      closes("s2", "s1"),
      closes("s1"),
    ]);

    expect(flattenTrajectories(nodes).map((e) => [e.node.trajectory, e.depth])).toEqual([
      ["s1", 0],
      ["s2", 1],
    ]);
  });
});
