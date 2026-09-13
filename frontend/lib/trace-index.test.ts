import { describe, expect, it } from "vitest";
import type { StreamEvent, TurnWithEvents } from "@/lib/api-types";
import { buildSessionTrace, projectSessionTurns } from "@/lib/trace-index";

let seq = 0;
function event(partial: Partial<StreamEvent> & { kind: string }): StreamEvent {
  return {
    seq: ++seq,
    stability: "stable",
    source: "agent",
    createdAtMs: seq,
    ...partial,
  };
}

function turn(id: string, events: StreamEvent[], origin: "user" | "agent" = "user") {
  return {
    turn: {
      id,
      sessionId: "local:codex:native",
      status: "completed",
      origin,
      requestText: origin === "user" ? "run it" : "",
      commandInvocationId: null,
      idempotencyKey: null,
      errorCode: null,
      errorMessage: null,
      createdAtMs: 1,
      startedAtMs: 1,
      completedAtMs: 2,
      stoppedAtMs: null,
    },
    events,
  } satisfies TurnWithEvents;
}

describe("buildSessionTrace", () => {
  it("keeps a parent's state independent from a completed nested trajectory", () => {
    const index = buildSessionTrace([turn("t1", [
      event({ kind: "subagent", phase: "started", streamId: "parent", interiorState: "unreadable" }),
      event({ kind: "subagent", phase: "completed", streamId: "child", span: "parent", interiorState: "full" }),
    ])]);

    expect(index.trajectories.get("parent")?.status).toBe("running");
    expect(index.trajectories.get("parent")?.interiorState).toBe("unreadable");
    expect(index.trajectories.get("child")?.status).toBe("completed");
  });

  it("does not infer a complete interior or a vendor refusal from observed row counts", () => {
    const index = buildSessionTrace([turn("t1", [
      event({ kind: "subagent", phase: "completed", streamId: "empty" }),
      event({ kind: "subagent", phase: "started", streamId: "parent" }),
      event({ kind: "assistant", phase: "completed", streamId: "answer", span: "parent", text: "a prefix" }),
    ])]);

    expect(index.trajectories.get("empty")?.interiorState).toBe("pending");
    expect(index.trajectories.get("parent")?.interiorState).toBe("partial");
  });

  it("moves a late child close back to the turn that owns the trajectory", () => {
    const host = event({ kind: "subagent", phase: "started", streamId: "child", name: "worker" });
    const childTool = event({
      kind: "tool_call",
      phase: "completed",
      streamId: "exec",
      span: "child",
      name: "echo",
    });
    const childClose = event({
      kind: "subagent",
      phase: "completed",
      streamId: "child",
      text: "done",
      arrivalTurnId: "agent-turn",
    });
    const parent = turn("parent-turn", [host, childTool]);
    const agent = turn("agent-turn", [], "agent");
    const index = buildSessionTrace([parent, agent], [childClose]);

    expect(index.trajectories.get("child")?.ownerTurnId).toBe("parent-turn");
    expect(index.trajectories.get("child")?.arrivalTurnIds).toEqual([
      "parent-turn",
      "agent-turn",
    ]);
    expect(index.eventsByTurn.get("parent-turn")?.map((item) => item.seq)).toEqual([
      host.seq,
      childTool.seq,
      childClose.seq,
    ]);
    expect(index.eventsByTurn.get("agent-turn")).toBeUndefined();

    const projected = projectSessionTurns([parent, agent], [childClose]);
    expect(projected.map((entry) => entry.turn.id)).toEqual(["parent-turn"]);
  });

  it("treats a tool host named by span as a promoted trajectory", () => {
    const host = event({
      kind: "tool_call",
      phase: "completed",
      streamId: "workflow",
      name: "Workflow",
    });
    const child = event({
      kind: "assistant",
      phase: "completed",
      streamId: "answer",
      span: "workflow",
      text: "child result",
    });
    const index = buildSessionTrace([turn("t1", [host, child])]);
    const trajectory = index.trajectories.get("workflow");

    expect(trajectory?.ownerTurnId).toBe("t1");
    expect(trajectory?.name).toBe("Workflow");
    expect(trajectory?.interiorState).toBe("partial");
    expect(trajectory?.events.map((item) => item.seq)).toEqual([host.seq, child.seq]);
  });

  it("does not let a child tool name replace the trajectory label", () => {
    const host = event({
      kind: "tool_call",
      phase: "completed",
      streamId: "workflow",
      name: "Workflow",
    });
    const child = event({
      kind: "tool_call",
      phase: "completed",
      streamId: "exec",
      span: "workflow",
      name: "Bash",
    });
    const index = buildSessionTrace([turn("t1", [host, child])]);

    expect(index.trajectories.get("workflow")?.name).toBe("Workflow");
  });

  it("leaves an unknown span on its arrival turn instead of dropping it", () => {
    const orphan = event({
      kind: "assistant",
      phase: "completed",
      streamId: "msg",
      span: "missing",
      text: "still visible",
    });
    const projected = projectSessionTurns([turn("t1", []), turn("t2", [orphan])]);
    expect(projected[1].events[0].text).toBe("still visible");
  });

  it("deduplicates the same sequence when history and the live trace overlap", () => {
    const item = event({ kind: "assistant", phase: "completed", text: "once" });
    const duplicate = { ...item, arrivalTurnId: "t1" };
    const projected = projectSessionTurns([turn("t1", [item])], [duplicate]);
    expect(projected[0].events).toHaveLength(1);
  });

  it("keeps producer-authored terminal interior states distinct", () => {
    const pending = event({
      kind: "subagent",
      phase: "started",
      streamId: "agent",
      interiorState: "pending",
    });
    const unreadable = event({
      kind: "subagent",
      phase: "delta",
      streamId: "agent",
      interiorState: "unreadable",
    });
    const index = buildSessionTrace([turn("t1", [pending, unreadable])]);
    expect(index.trajectories.get("agent")?.interiorState).toBe("unreadable");

    const hollow = event({
      kind: "subagent",
      phase: "completed",
      streamId: "hollow",
      interiorState: "forwards_nothing",
    });
    const hollowIndex = buildSessionTrace([turn("t2", [hollow])]);
    expect(hollowIndex.trajectories.get("hollow")?.interiorState).toBe("forwards_nothing");
  });
});
