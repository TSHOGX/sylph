import { describe, expect, it } from "vitest";
import type { StreamEvent } from "@/lib/api-types";
import { terminalStateOf } from "@/lib/use-sse";

/**
 * Reading a turn's outcome off its own event stream.
 *
 * The bug this file exists to prevent: the backend has always announced a turn's close on
 * the turn's stream — `TurnCloser::close` emits a `status` frame carrying
 * `{"state": status}` — and the client threw it away, because `StatusLine` renders
 * `event.text` and that frame's text is empty by design. So the page polled
 * `GET /turns/{id}` every 1500ms for a fact it was already being handed, and settled up to
 * a poll interval late.
 *
 * The frame is also *stricter* than the row it replaced. The turn row commits before the
 * settle sweep closes whatever trajectories the turn left open; this frame is emitted
 * after. That is why the state has to be read here rather than inferred from the stream
 * simply ending (`AGENTS.md` § And `Stop` disappearing is still the wrong signal).
 */
function frame(over: Partial<StreamEvent>): StreamEvent {
  return {
    seq: 1,
    kind: "status",
    stability: "stable",
    source: "server",
    text: "",
    isError: false,
    createdAtMs: 1_700_000_000_000,
    ...over,
  } as StreamEvent;
}

describe("terminalStateOf", () => {
  it("reads the state out of a status frame's data", () => {
    // The regression: this is the frame the client used to discard.
    expect(terminalStateOf(frame({ data: { state: "completed" } }))).toBe("completed");
    expect(terminalStateOf(frame({ data: { state: "failed" } }))).toBe("failed");
    expect(terminalStateOf(frame({ data: { state: "cancelled" } }))).toBe("cancelled");
  });

  it("ignores a status frame that reports a state the turn can still leave", () => {
    // `running` and `queued` travel on this same frame kind. Treating any status frame as
    // a close would settle a turn the moment it started.
    expect(terminalStateOf(frame({ data: { state: "running" } }))).toBeNull();
    expect(terminalStateOf(frame({ data: { state: "queued" } }))).toBeNull();
    expect(terminalStateOf(frame({ data: { state: "stopping" } }))).toBeNull();
  });

  it("ignores every other event kind", () => {
    // A `tool_result` may legitimately carry a `state` in its payload, and an `error`
    // frame precedes the close rather than being it.
    expect(terminalStateOf(frame({ kind: "assistant", data: { state: "completed" } }))).toBeNull();
    expect(terminalStateOf(frame({ kind: "tool_result", data: { state: "completed" } }))).toBeNull();
    expect(terminalStateOf(frame({ kind: "error", isError: true }))).toBeNull();
  });

  it("survives a status frame with no usable data", () => {
    // Absent, null, and wrong-typed all have to read as "not a close" rather than throw:
    // one malformed frame must not stop a live stream from settling on the next one.
    expect(terminalStateOf(frame({ data: undefined }))).toBeNull();
    expect(terminalStateOf(frame({ data: null as unknown as undefined }))).toBeNull();
    expect(terminalStateOf(frame({ data: {} }))).toBeNull();
    expect(terminalStateOf(frame({ data: { state: 7 } }))).toBeNull();
    expect(terminalStateOf(frame({ data: { state: null } }))).toBeNull();
  });
});
