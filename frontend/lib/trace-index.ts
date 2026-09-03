import type { InteriorState, StreamEvent, TurnWithEvents } from "@/lib/api-types";
import { mergeInteriorState } from "@/lib/interior-state";

/** A server event with its arrival turn made explicit for session projections. */
export interface TraceEvent extends StreamEvent {
  arrivalTurnId: string;
}

export type TraceStatus = "running" | "completed" | "failed" | "abandoned";

/** The durable identity and observed facts for one delegated trajectory. */
export interface TraceTrajectory {
  id: string;
  parentId?: string;
  ownerTurnId: string;
  arrivalTurnIds: string[];
  events: TraceEvent[];
  status: TraceStatus;
  name?: string;
  /** Producer-authored reachability state for this trajectory's interior. */
  interiorState: InteriorState;
}

export interface SessionTraceIndex {
  events: TraceEvent[];
  trajectories: Map<string, TraceTrajectory>;
  /** Events projected onto the turn that owns their trajectory. */
  eventsByTurn: Map<string, TraceEvent[]>;
}

/**
 * Build a session-scoped trajectory index from history and live trace events.
 *
 * `span` describes lineage; it does not say which turn owns a trajectory. The first
 * declaration (or the first event of a promoted host) establishes `ownerTurnId`, while
 * `arrivalTurnId` records where the server actually received each later event. Keeping
 * the two facts separate is what makes a Codex child close that arrives after its parent
 * turn finished update the original card instead of creating an `unprompted` root turn.
 */
export function buildSessionTrace(
  turns: TurnWithEvents[],
  liveEvents: StreamEvent[] = [],
): SessionTraceIndex {
  const bySeq = new Map<number, TraceEvent>();

  for (const entry of turns) {
    for (const event of entry.events) {
      const normalized = withArrivalTurn(event, entry.turn.id);
      // The session allocator makes seq globally unique. Prefer a trace copy when it
      // carries the explicit server field; this also makes the index tolerant of a
      // mixed-version server during rollout.
      const existing = bySeq.get(normalized.seq);
      if (!existing || !existing.arrivalTurnId) bySeq.set(normalized.seq, normalized);
    }
  }
  for (const event of liveEvents) {
    const normalized = withArrivalTurn(event, event.arrivalTurnId);
    if (!normalized.arrivalTurnId) continue;
    bySeq.set(normalized.seq, normalized);
  }

  const events = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  const referenced = new Set(
    events.map((event) => event.span).filter((id): id is string => Boolean(id)),
  );
  const trajectoryIds = new Set<string>(referenced);
  for (const event of events) {
    if (event.kind === "subagent" && event.streamId) trajectoryIds.add(event.streamId);
  }

  const trajectories = new Map<string, TraceTrajectory>();
  for (const id of trajectoryIds) {
    const own = events.filter((event) => event.streamId === id || event.span === id);
    if (own.length === 0) continue;
    const declaration = own.find(
      (event) => event.kind === "subagent" && event.streamId === id,
    );
    const opening = declaration ?? own[0];
    // A trajectory's label comes from its own declaration/host row only. Child
    // tool calls carry names too, but letting the last descendant win turns a
    // `Workflow` into `Bash` as soon as its first worker runs — a plausible value
    // that silently changes the card the user is looking at.
    const names = own
      .filter((event) => event.streamId === id)
      .map((event) => event.name?.trim())
      .filter((name): name is string => Boolean(name));
    const close = [...own]
      .reverse()
      .find((event) => event.phase === "completed" && event.kind === "subagent");
    const serverClosed = Boolean(
      close?.data &&
        typeof close.data === "object" &&
        (close.data as { closedBy?: unknown }).closedBy === "server",
    );
    const failed = own.some((event) => event.isError);
    const status: TraceStatus =
      serverClosed
        ? "abandoned"
        : failed
          ? "failed"
          : close
            ? "completed"
            : "running";
    const arrivalTurnIds = [...new Set(own.map((event) => event.arrivalTurnId))];
    const explicitState = own
      .filter((event) => event.kind === "subagent" && event.interiorState)
      .map((event) => event.interiorState!)
      .reduce<InteriorState | undefined>(
        (current, incoming) => mergeInteriorState(current, incoming),
        undefined,
      );
    const hasInterior = own.some((event) => event.kind !== "subagent" && event.span === id);
    // Old rows predate the typed state. Preserve their only reliable distinctions while
    // making the fallback explicit: observed child events mean full, a still-live trajectory
    // may acquire a source later, and a closed childless trajectory is the old vendor contract
    // that forwards nothing.
    const interiorState =
      explicitState ??
      (hasInterior ? "full" : status === "running" || status === "abandoned" ? "pending" : "forwards_nothing");

    trajectories.set(id, {
      id,
      parentId: declaration?.span ?? opening.span,
      ownerTurnId: opening.arrivalTurnId,
      arrivalTurnIds,
      events: own,
      status,
      name: names[names.length - 1],
      interiorState,
    });
  }

  const eventsByTurn = new Map<string, TraceEvent[]>();
  for (const event of events) {
    const trajectory = trajectoryForEvent(event, trajectories);
    const targetTurn = trajectory?.ownerTurnId ?? event.arrivalTurnId;
    const bucket = eventsByTurn.get(targetTurn);
    if (bucket) bucket.push(event);
    else eventsByTurn.set(targetTurn, [event]);
  }

  return { events, trajectories, eventsByTurn };
}

/**
 * Project a session trace back onto the turn list used by the existing transcript UI.
 * Late child events disappear from the agent-created arrival turn and are appended to
 * the turn that owns their trajectory. Unknown spans remain on arrival, preserving work
 * when a declaration was lost or an older backend did not persist one.
 */
export function projectSessionTurns(
  turns: TurnWithEvents[],
  liveEvents: StreamEvent[] = [],
): TurnWithEvents[] {
  return projectSessionTurnsFromIndex(turns, buildSessionTrace(turns, liveEvents));
}

/** Project turns using an index the caller already built for the same snapshot. */
export function projectSessionTurnsFromIndex(
  turns: TurnWithEvents[],
  index: SessionTraceIndex,
): TurnWithEvents[] {
  return turns
    .map((entry) => ({
      ...entry,
      events: (index.eventsByTurn.get(entry.turn.id) ?? []).sort((a, b) => a.seq - b.seq),
    }))
    // A late child close may be the only event that arrived on an agent-created
    // reporting turn. Once that event is projected back to its owner, retaining an
    // empty `unprompted` section adds a second, blank card and makes the user think
    // the report itself was lost. A non-empty agent turn remains visible because it
    // carries the agent's own narration, which is a legitimate turn-level result.
    .filter((entry) => entry.turn.origin !== "agent" || entry.events.length > 0);
}

function withArrivalTurn(event: StreamEvent, fallback?: string): TraceEvent {
  return {
    ...event,
    arrivalTurnId: event.arrivalTurnId ?? fallback ?? "",
  };
}

function trajectoryForEvent(
  event: TraceEvent,
  trajectories: Map<string, TraceTrajectory>,
): TraceTrajectory | undefined {
  if (event.streamId) {
    const own = trajectories.get(event.streamId);
    if (own) return own;
  }
  if (event.span) return trajectories.get(event.span);
  return undefined;
}
