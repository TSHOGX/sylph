import { describe, expect, it } from "vitest";
import {
  buildRows,
  buildTree,
  flattenTrajectories,
  pathTo,
  trajectoryName,
  trajectoryReport,
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

/**
 * Promotion: a trajectory is defined by use, not by declaration.
 *
 * `docs/session-protocol.md` § Promotion. These pin the four defects that made two
 * real delegations render as permanently-running cards with no visible interior:
 * a host nobody declared stayed flat, N delegates under one host collapsed into
 * one row, a promoted host's status was read off the wrong field, and nothing ever
 * closed it.
 */
describe("promotion", () => {
  function call(id: string, name: string, span?: string): StreamEvent {
    return ev({ kind: "tool_call", phase: "completed", streamId: id, name, span });
  }
  function result(id: string, isError?: boolean): StreamEvent {
    return ev({ kind: "tool_result", phase: "completed", streamId: id, isError });
  }

  it("makes a tool call its children name into their container", () => {
    // The root defect. `Agent` and `Workflow` are delegating tools the adapter
    // never whitelisted, so their children were rooted beside the call instead of
    // inside it — the delegation read as flat and its work looked like the root
    // agent's own.
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
    ]);

    expect(nodes).toHaveLength(1);
    expect(nodes[0].trajectory).toBe("h1");
    expect(nodes[0].children.map((c) => c.row.event.streamId)).toEqual(["b1"]);
  });

  it("keeps a promoted host a tool call rather than rewriting it", () => {
    // Its name, arguments, and card are the tool's. Folding a trajectory identity
    // onto it would discard all three.
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
    ]);

    expect(nodes[0].row.event.kind).toBe("tool_call");
    expect(nodes[0].row.event.name).toBe("Workflow");
  });

  it("gives every delegate under one host its own row", () => {
    // Measured on a real session: a two-agent workflow rendered as a single card
    // reading "Demo: agent-2" because both delegates were keyed by the host's id,
    // and the last name to arrive won. The other agent was simply absent.
    const nodes = tree([
      call("h1", "Workflow"),
      opens("t1", "Demo: agent-1", "h1"),
      opens("t2", "Demo: agent-2", "h1"),
    ]);

    expect(nodes).toHaveLength(1);
    expect(nodes[0].children.map((c) => c.row.event.name)).toEqual([
      "Demo: agent-1",
      "Demo: agent-2",
    ]);
  });

  it("reports a promoted host as running until its result arrives", () => {
    // A tool call's `completed` phase means its arguments finished streaming. Read
    // as the trajectory's lifecycle it reports every delegation as finished the
    // instant its input parsed.
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
    ]);

    expect(nodes[0].status).toBe("running");
  });

  it("closes a promoted host with its own tool result", () => {
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
      result("h1"),
    ]);

    expect(nodes[0].status).toBe("done");
  });

  it("fails a promoted host whose result reports an error", () => {
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
      result("h1", true),
    ]);

    expect(nodes[0].status).toBe("failed");
  });

  it("reports a promoted host the server closed as abandoned, not failed", () => {
    // The server closes what an adapter left open. Nobody reported an outcome, so
    // colouring it red would report an interrupted turn as a broken one.
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "h1",
        isError: true,
        data: { closedBy: "server", reason: "abandoned" },
      }),
    ]);

    expect(nodes[0].status).toBe("abandoned");
    expect(nodes[0].row.event.kind).toBe("tool_call");
  });

  it("does not grow a second row when a promoted host is closed late", () => {
    // The close arrives after the `tool_call` left `openBlocks`, so without a
    // durable index it became a second root row and the delegation showed twice.
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "h1",
        data: { closedBy: "server" },
      }),
    ]);

    expect(nodes).toHaveLength(1);
  });

  it("does not fail a host whose only unresolved delegate was abandoned", () => {
    // Measured on a real session: a `Workflow` whose own `tool_result` succeeded was
    // badged `failed` because one delegate was still running when the turn ended.
    // The server marks its own closing event `isError` so the row reads as
    // unresolved rather than clean, and the rollup counted that as a descendant
    // failure — escalating every ancestor. `abandoned` means nobody reported an
    // outcome, which is a different fact from work that reported an error, and
    // conflating them reports an interrupted turn as a broken one.
    const nodes = tree([
      call("h1", "Workflow"),
      opens("t1", "Demo: agent-1", "h1"),
      result("h1"),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "t1",
        span: "h1",
        isError: true,
        data: { closedBy: "server", reason: "abandoned" },
      }),
    ]);

    expect(nodes[0].status).toBe("done");
    expect(nodes[0].rollup.failures).toBe(0);
    expect(nodes[0].children[0].status).toBe("abandoned");
  });

  it("replaces a progress snapshot instead of appending it", () => {
    // These notifications re-send the whole summary rather than a suffix. Appended
    // as delta text, one sentence was stored six times.
    const nodes = tree([
      call("h1", "Workflow"),
      ev({ kind: "subagent", phase: "delta", streamId: "h1", data: { progress: "step 1" } }),
      ev({ kind: "subagent", phase: "delta", streamId: "h1", data: { progress: "step 2" } }),
      ev({ kind: "assistant", phase: "completed", streamId: "b1", span: "h1", text: "x" }),
    ]);

    expect(nodes).toHaveLength(1);
    expect((nodes[0].row.event.data as { progress?: string }).progress).toBe("step 2");
  });

  it("roots a row that names itself rather than parenting it", () => {
    // A node held as its own child is unreachable from the root, and the rollup
    // would recurse on it forever.
    const nodes = tree([
      ev({ kind: "subagent", phase: "started", streamId: "s1", span: "s1" }),
    ]);

    expect(nodes).toHaveLength(1);
    expect(nodes[0].trajectory).toBe("s1");
  });

  it("survives two trajectories naming each other", () => {
    // No single-pass attachment can detect this: the first waits as an orphan for
    // the second, which then adopts it and is adopted by it. Unbounded recursion in
    // a render path is a frozen tab.
    const nodes = tree([
      ev({ kind: "subagent", phase: "started", streamId: "s1", span: "s2" }),
      ev({ kind: "subagent", phase: "started", streamId: "s2", span: "s1" }),
    ]);

    expect(nodes.length).toBeGreaterThan(0);
    expect(nodes[0].rollup.events).toBeLessThan(10);
  });

  it("reuses the node for a row that did not change", () => {
    // What lets the memoized renderer skip settled history while a turn streams. A
    // fresh node per build fails every memo comparison, so a fan-out re-renders its
    // whole subtree at token rate.
    const settled = [opens("s1", "Explore"), closes("s1")];
    const rows = buildRows(settled);
    const first = buildTree(rows);
    const second = buildTree(buildRows(settled.concat(
      ev({ kind: "assistant", phase: "completed", streamId: "late", text: "tail" }),
    )));

    expect(second[0]).toBe(first[0]);
  });
});

/**
 * What a collapsed delegation can say about itself.
 *
 * The backend records a delegate's summary and the card showed neither it nor the
 * progress snapshot, so the one line of *content* a collapsed row could carry was
 * stored and never rendered. These pin the two rules that make reading it safe, both of
 * which fail silently: a promoted host's `text` is its tool arguments, and a progress
 * snapshot describes work a close has already superseded.
 */
describe("trajectoryName", () => {
  it("prefers the name the agent gave", () => {
    const nodes = tree([opens("s1", "code-reviewer"), closes("s1")]);
    expect(trajectoryName(nodes[0])).toBe("code-reviewer");
  });

  it("falls back for a trajectory with no name, and treats blank as none", () => {
    // The server's own close carries no name at all, and an adapter that declares a
    // trajectory before its brief parses now sends none either — so this path is the
    // common one rather than a corner.
    expect(trajectoryName(tree([opens("s1"), closes("s1")])[0])).toBe("Unnamed");
    expect(trajectoryName(tree([opens("s2", "   "), closes("s2")])[0])).toBe("Unnamed");
  });

  it("uses a fallback no tool name is a substring of", () => {
    // Not a style rule. The previous fallback was "Subagent", and "Subagent delegation"
    // *contains* "agent delegation" — while `Agent` is a tool `claude` ships — so every
    // `{ name: "<tool> delegation" }` locator was ambiguous with it by construction, and
    // only in the states that produce a nameless row. That passed indefinitely and then
    // failed under load, twice, reading as a broken card rather than a loose locator.
    // Asserted against the tool names that actually reach this label.
    const fallback = trajectoryName(tree([opens("s1"), closes("s1")])[0]);
    const label = `${fallback} delegation`;
    for (const tool of ["Task", "Agent", "Workflow", "Bash", "Read", "Edit"]) {
      expect(
        label.toLowerCase(),
        `"${label}" must not contain "${tool} delegation"`,
      ).not.toContain(`${tool.toLowerCase()} delegation`);
    }
  });

  it("is the single definition both naming surfaces use", () => {
    // The card's region label and the overview row's accessible name are the two places
    // one node is named, and each used to compute the fallback itself. Two derivations of
    // one fact diverge the moment either grows a case — and the failure is silent: the
    // strip would name a node one way while the card named it another, so a locator that
    // finds one stops finding the other. Asserted as an invariant over the shape both
    // consume, since a `node` test environment cannot render either surface.
    const named = tree([opens("s1", "Explore"), closes("s1")])[0];
    const bare = tree([opens("s2"), closes("s2")])[0];
    for (const node of [named, bare]) {
      expect(trajectoryName(node)).toBe(node.row.event.name?.trim() || "Unnamed");
      expect(trajectoryName(node).trim()).not.toBe("");
    }
  });
});

describe("trajectoryReport", () => {
  it("reports a declared delegation's closing summary", () => {
    const nodes = tree([
      opens("s1", "Explore"),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "s1",
        text: "Found 3 call sites.",
      }),
    ]);

    expect(trajectoryReport(nodes[0])).toBe("Found 3 call sites.");
  });

  it("prefers a close over the progress snapshot it supersedes", () => {
    // Progress is a snapshot of work in flight. Once an outcome is reported the
    // snapshot describes a state that no longer holds, so a delegation that finished
    // would otherwise be captioned with its own last guess.
    const nodes = tree([
      opens("s1", "Verify"),
      ev({
        kind: "subagent",
        phase: "delta",
        streamId: "s1",
        data: { progress: "Working on it" },
      }),
      ev({ kind: "subagent", phase: "completed", streamId: "s1", text: "All 12 pass." }),
    ]);

    expect(trajectoryReport(nodes[0])).toBe("All 12 pass.");
  });

  it("falls back to progress while a delegation is still running", () => {
    const nodes = tree([
      opens("s1", "Verify"),
      ev({
        kind: "subagent",
        phase: "delta",
        streamId: "s1",
        data: { progress: "Working on it" },
      }),
    ]);

    expect(trajectoryReport(nodes[0])).toBe("Working on it");
  });

  it("never reports a promoted host's arguments as its summary", () => {
    // A promoted host's `text` is the tool call's serialized input, so reading `text`
    // uniformly puts a JSON blob where a sentence belongs. Its outcome arrives as the
    // `subagent` close held *beside* the row, which is what `trajectoryClose` is for.
    const nodes = tree([
      ev({
        kind: "tool_call",
        phase: "completed",
        streamId: "host",
        name: "Workflow",
        text: JSON.stringify({ script: "parallel([a, b])" }),
      }),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "host",
        text: "both agents done",
      }),
    ]);

    const host = nodes.find((node) => node.trajectory === "host")!;
    expect(host.row.event.kind).toBe("tool_call");
    expect(trajectoryReport(host)).toBe("both agents done");
    expect(trajectoryReport(host)).not.toContain("script");
  });

  it("never reports a promoted host's tool_result as its summary", () => {
    // The obvious next step for anyone extending this, because a promoted host's
    // lifecycle *is* its `tool_result` (§ Promotion) — so reading the result for its
    // summary too looks consistent. It is not: a result is the tool's output, not the
    // delegation's account of itself.
    //
    // And it is load-bearing. A card's text is matched by `not.toContainText(...)` in
    // `e2e/delegation.spec.ts`, so a status word inside a summary fails those as though
    // the status were wrong — a status bug three layers from the cause. The string here
    // is the real fixture's (`pr-host-d`), which contains "running" and is the exact
    // ammunition those assertions are exposed to.
    const nodes = tree([
      ev({
        kind: "tool_call",
        phase: "completed",
        streamId: "host",
        name: "Workflow",
        text: JSON.stringify({ script: "parallel([sleep20, sleep15])" }),
      }),
      ev({ kind: "subagent", phase: "started", streamId: "task", span: "host", name: "W" }),
      ev({
        kind: "tool_result",
        phase: "completed",
        streamId: "host",
        name: "Workflow",
        text: "dispatched; running in background",
      }),
    ]);

    const host = nodes.find((node) => node.trajectory === "host")!;
    expect(host.row.result?.text).toBe("dispatched; running in background");
    expect(trajectoryReport(host)).toBeUndefined();
  });

  it("reports nothing for a trajectory the server closed, rather than inventing one", () => {
    // The sweep's closing event carries `String::new()` deliberately: nobody reported an
    // outcome, and a card that filled that silence with the server's own close would be
    // stating one. Pinned because the close is a `subagent` event on the row, which is
    // exactly the shape a real summary has.
    const nodes = tree([
      ev({
        kind: "tool_call",
        phase: "completed",
        streamId: "host",
        name: "Workflow",
        text: JSON.stringify({ script: "phase('Stage A')" }),
      }),
      ev({ kind: "subagent", phase: "started", streamId: "task", span: "host", name: "A" }),
      ev({
        kind: "subagent",
        phase: "completed",
        streamId: "host",
        text: "",
        isError: true,
        data: { closedBy: "server", reason: "abandoned" },
      }),
    ]);

    const host = nodes.find((node) => node.trajectory === "host")!;
    expect(host.status).toBe("abandoned");
    expect(trajectoryReport(host)).toBeUndefined();
  });

  it("says nothing when a delegation reported nothing", () => {
    const nodes = tree([opens("s1", "Quiet"), closes("s1")]);
    expect(trajectoryReport(nodes[0])).toBeUndefined();
  });

  it("ignores a blank or non-string report rather than rendering an empty line", () => {
    const blank = tree([
      opens("s1", "Blank"),
      ev({ kind: "subagent", phase: "completed", streamId: "s1", text: "   " }),
    ]);
    expect(trajectoryReport(blank[0])).toBeUndefined();

    const wrongType = tree([
      opens("s2", "Odd"),
      ev({ kind: "subagent", phase: "delta", streamId: "s2", data: { progress: 42 } }),
    ]);
    expect(trajectoryReport(wrongType[0])).toBeUndefined();
  });
});
