import { describe, expect, it } from "vitest";
import { buildRows } from "@/components/timeline";
import type { StreamEvent } from "@/lib/api-types";

/**
 * Collapsing a raw event stream into timeline rows.
 *
 * Two invariants matter and both have visible failure modes: showing a token
 * twice (delta plus completed), and separating a tool call from its result so one
 * operation reads as two log lines. Row identity is a third: the memoized
 * renderer compares rows by reference, so a row that is rebuilt when its content
 * did not change silently costs a re-render of settled history.
 */
let seq = 0;
function ev(partial: Partial<StreamEvent> & { kind: string }): StreamEvent {
  return {
    seq: (seq += 1),
    stability: "stable",
    source: "runner",
    createdAtMs: 0,
    ...partial,
  };
}

describe("buildRows", () => {
  it("accumulates deltas and replaces them with the completed payload", () => {
    const rows = buildRows([
      ev({ kind: "assistant", phase: "started", streamId: "a" }),
      ev({ kind: "assistant", phase: "delta", streamId: "a", text: "Hello " }),
      ev({ kind: "assistant", phase: "delta", streamId: "a", text: "world" }),
      ev({ kind: "assistant", phase: "completed", streamId: "a", text: "Hello world" }),
    ]);
    // One row, not four: the block is one thought that arrived in pieces.
    expect(rows).toHaveLength(1);
    expect(rows[0].event.text).toBe("Hello world");
    expect(rows[0].event.phase).toBe("completed");
  });

  it("shows accumulated text while a block is still streaming", () => {
    const rows = buildRows([
      ev({ kind: "assistant", phase: "started", streamId: "a" }),
      ev({ kind: "assistant", phase: "delta", streamId: "a", text: "par" }),
      ev({ kind: "assistant", phase: "delta", streamId: "a", text: "tial" }),
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0].event.text).toBe("partial");
  });

  it("skips a started event with no text so no empty row appears", () => {
    const rows = buildRows([ev({ kind: "assistant", phase: "started", streamId: "a" })]);
    expect(rows).toEqual([]);
  });

  it("shows a started tool call immediately, even with no text", () => {
    // The exception to the rule above: a call that has begun is worth showing,
    // because it is how the user sees the agent is doing something.
    const rows = buildRows([
      ev({ kind: "tool_call", phase: "started", streamId: "t", name: "Bash" }),
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0].event.name).toBe("Bash");
  });

  it("pairs a tool result with the call sharing its id", () => {
    const rows = buildRows([
      ev({ kind: "tool_call", phase: "completed", streamId: "t1", name: "Read", text: "{}" }),
      ev({ kind: "tool_result", phase: "completed", streamId: "t1", text: "file contents" }),
    ]);
    // One row carrying both halves, so the call and its output render as one
    // operation rather than two entries.
    expect(rows).toHaveLength(1);
    expect(rows[0].event.kind).toBe("tool_call");
    expect(rows[0].result?.text).toBe("file contents");
  });

  it("appends result deltas and lets a completed result replace them", () => {
    const rows = buildRows([
      ev({ kind: "tool_call", phase: "completed", streamId: "t1", name: "Bash" }),
      ev({ kind: "tool_result", phase: "delta", streamId: "t1", text: "line 1\n" }),
      ev({ kind: "tool_result", phase: "delta", streamId: "t1", text: "line 2\n" }),
    ]);
    expect(rows[0].result?.text).toBe("line 1\nline 2\n");

    const finished = buildRows([
      ev({ kind: "tool_call", phase: "completed", streamId: "t2", name: "Bash" }),
      ev({ kind: "tool_result", phase: "delta", streamId: "t2", text: "partial" }),
      ev({ kind: "tool_result", phase: "completed", streamId: "t2", text: "full output" }),
    ]);
    expect(finished[0].result?.text).toBe("full output");
  });

  it("renders an unpaired result as its own row rather than dropping it", () => {
    // A runner that does not reuse the call id still gets its output shown.
    const rows = buildRows([
      ev({ kind: "tool_call", phase: "completed", streamId: "call-1", name: "Read" }),
      ev({ kind: "tool_result", phase: "completed", streamId: "other", text: "orphaned" }),
    ]);
    expect(rows).toHaveLength(2);
    expect(rows[1].event.kind).toBe("tool_result");
    expect(rows[1].event.text).toBe("orphaned");
  });

  it("keeps several interleaved streams apart", () => {
    const rows = buildRows([
      ev({ kind: "thinking", phase: "delta", streamId: "think", text: "reason " }),
      ev({ kind: "assistant", phase: "delta", streamId: "say", text: "answer " }),
      ev({ kind: "thinking", phase: "delta", streamId: "think", text: "more" }),
      ev({ kind: "assistant", phase: "delta", streamId: "say", text: "text" }),
    ]);
    expect(rows).toHaveLength(2);
    expect(rows[0].event.text).toBe("reason more");
    expect(rows[1].event.text).toBe("answer text");
  });

  it("gives every row a distinct key", () => {
    const rows = buildRows([
      ev({ kind: "user", text: "ask" }),
      ev({ kind: "assistant", phase: "completed", streamId: "a", text: "reply" }),
      ev({ kind: "tool_call", phase: "completed", streamId: "t", name: "Read" }),
      ev({ kind: "status", text: "done" }),
    ]);
    const keys = rows.map((row) => row.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("leaves settled rows referentially stable as new events arrive", () => {
    // This is what the memoized renderer relies on: appending an event must not
    // rebuild earlier rows, or every token re-renders the whole conversation.
    const settled = [
      ev({ kind: "user", text: "ask" }),
      ev({ kind: "assistant", phase: "completed", streamId: "a", text: "first" }),
    ];
    const before = buildRows(settled);
    const after = buildRows([
      ...settled,
      ev({ kind: "assistant", phase: "delta", streamId: "b", text: "second" }),
    ]);

    // The event objects are shared, which is what `memo`'s comparator checks.
    expect(after[0].event).toBe(before[0].event);
    expect(after[1].event).toBe(before[1].event);
    expect(after).toHaveLength(3);
  });

  it("handles events with no streamId at all", () => {
    const rows = buildRows([
      ev({ kind: "user", text: "one" }),
      ev({ kind: "status", text: "two" }),
      ev({ kind: "usage", data: { inputTokens: 1 } }),
    ]);
    expect(rows).toHaveLength(3);
  });

  it("returns nothing for an empty stream", () => {
    expect(buildRows([])).toEqual([]);
  });
});
