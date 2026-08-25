/**
 * A turn's event stream, as the tree of delegated trajectories it actually is.
 *
 * # Why a tree, and why it is not indentation
 *
 * An agent delegates; its delegates delegate; a dynamic workflow fans out to a
 * dozen agents at once and then synthesizes. `docs/session-protocol.md`
 * § Delegated trajectories carries that with one field — an event's `span` names
 * the trajectory it ran in, and a `subagent` event's `streamId` *is* a trajectory
 * id. Depth is unbounded and siblings are concurrent.
 *
 * The naive rendering is one indent level per depth, and it fails on contact: at
 * depth 4 there is no usable width left on a phone, and at twenty concurrent
 * delegates the screen is a wall of near-identical rows. Every mature tool that
 * faced this stopped *displaying* the tree and started letting it be *navigated*
 * — a file manager, a profiler's flame graph, a CI run's job list. So this module
 * produces the structure, and the renderer shows one level at a time: a
 * trajectory is one collapsed row until opened, and opening it is the user
 * saying which branch they care about.
 *
 * That is also what makes it fast. A collapsed trajectory renders no
 * descendants, so DOM cost tracks what the user opened rather than what the
 * agent did — a 20,000-event workflow costs the same as a small one until
 * someone drills in. Indentation cannot have that property, because it must lay
 * out every node to know where any node goes.
 *
 * # Rollups exist because collapsing hides failure
 *
 * A collapsed row that says only "Subagent" is a place a failure three levels
 * down goes to die. So each node carries an aggregate of its whole subtree —
 * failures, tool count, token usage, duration — computed bottom-up. The rule is
 * that no state is reachable in the tree that is not reported at the root, or
 * collapsing would be lossy rather than deferred.
 */

import type { StreamEvent } from "@/lib/api";

/** A displayable row: an event, plus the result paired to it when it is a call. */
export interface TimelineRow {
  event: StreamEvent;
  /** Result paired to this row, when it is a tool call. */
  result?: StreamEvent;
  /**
   * When a trajectory closed. Held beside the event rather than in it, because
   * merging phases keeps the *opening* `createdAtMs` — that is the row's start —
   * so the closing timestamp has nowhere else to go, and without it a delegation
   * cannot report how long it took.
   */
  closedAtMs?: number;
  key: string;
}

/** What a trajectory's subtree adds up to. */
export interface Rollup {
  /** Events anywhere beneath this node, trajectories included. */
  events: number;
  /** Trajectories anywhere beneath it, at any depth. */
  trajectories: number;
  /** Completed tool calls anywhere beneath it. */
  tools: number;
  /** Failures anywhere beneath it: failed tools, errors, failed trajectories. */
  failures: number;
  /** Deepest nesting below this node. A leaf is 0. */
  depth: number;
  /** Summed token usage, when any descendant reported it. */
  tokens?: { input: number; output: number };
  /** Wall time from this trajectory's start to its close, while known. */
  durationMs?: number;
}

/**
 * One node: a row, and — when that row opened a trajectory — its children.
 *
 * A plain event and a trajectory are the same type on purpose. The renderer walks
 * one list and asks whether a node has children, rather than interleaving two
 * collections and having to decide their relative order, which is where an
 * "events, then subagents" split silently reorders a turn.
 */
export interface TimelineNode {
  row: TimelineRow;
  /** The trajectory id this node opened, when it is a `subagent` node. */
  trajectory?: string;
  children: TimelineNode[];
  rollup: Rollup;
  /** Resolved lifecycle of a trajectory. `undefined` for a plain event. */
  status?: TrajectoryStatus;
  key: string;
}

/**
 * A trajectory's own outcome.
 *
 * `abandoned` is distinct from `failed` because they answer different questions:
 * failed means the delegated work reported an error, abandoned means nobody ever
 * said. The server closes open trajectories when a turn terminates precisely so
 * this is recorded rather than inferred, and conflating them would report an
 * interrupted turn as a broken one.
 */
export type TrajectoryStatus = "running" | "done" | "failed" | "abandoned";

const EMPTY_ROLLUP: Rollup = {
  events: 0,
  trajectories: 0,
  tools: 0,
  failures: 0,
  depth: 0,
};

/**
 * Collapse a raw event stream into displayable rows.
 *
 * Two transformations, both about not showing the same thing twice:
 *
 * 1. **Delta accumulation.** A `delta` carries only the newly arrived suffix and
 *    the matching `completed` carries the whole value, so rendering both shows
 *    every token twice. Deltas accumulate under their `streamId` and are replaced
 *    when `completed` arrives.
 * 2. **Result pairing.** A `tool_result` belongs to the `tool_call` that produced
 *    it — they share an id — so it attaches to its call rather than becoming a
 *    second row, which is what makes one operation read as one operation.
 *
 * A trajectory is the one block whose phases are not all interchangeable: its
 * `started` establishes identity, its `delta`s carry progress and the real name,
 * and only its `completed` states an outcome. So a `subagent` row keeps the
 * latest name it was given while its status comes from its phase, rather than
 * the last event winning outright.
 */
export function buildRows(events: StreamEvent[]): TimelineRow[] {
  const rows: TimelineRow[] = [];
  const openBlocks = new Map<string, number>();
  /** streamId -> index in `rows` of the tool call awaiting its result. */
  const callIndex = new Map<string, number>();

  for (const event of events) {
    const id = event.streamId;

    if (event.kind === "tool_result") {
      // Attach to the call sharing this id. Runners that do not reuse the id
      // fall through and the result renders as its own row, which is still
      // correct, just less tightly grouped.
      const at = id ? callIndex.get(id) : undefined;
      if (at !== undefined) {
        const existing = rows[at].result;
        rows[at] = {
          ...rows[at],
          // Deltas on a result append; a completed result replaces.
          result:
            event.phase === "delta" && existing
              ? { ...event, text: (existing.text ?? "") + (event.text ?? "") }
              : event,
        };
        continue;
      }
    }

    // A trajectory's row is updated in place across every phase, because the
    // three phases carry different facts about one node: identity, then progress
    // and name, then outcome. Replacing the row on `completed` — which is right
    // for a text block — would drop the name the brief supplied and leave every
    // card reading "Subagent".
    if (event.kind === "subagent" && id) {
      const at = openBlocks.get(id);
      if (at !== undefined) {
        const existing = rows[at].event;
        rows[at] = {
          ...rows[at],
          closedAtMs:
            event.phase === "completed" ? event.createdAtMs : rows[at].closedAtMs,
          event: {
            ...existing,
            phase: event.phase,
            // A later name is better informed: the agent's type is only known
            // once its streamed brief parses.
            name: event.name ?? existing.name,
            text: event.text || existing.text,
            data: event.data ?? existing.data,
            isError: event.isError || existing.isError,
            span: existing.span ?? event.span,
          },
        };
        continue;
      }
      openBlocks.set(id, rows.length);
      rows.push({ event: { ...event }, key: rowKey(event, rows.length) });
      continue;
    }

    if (event.phase === "delta" && id) {
      const at = openBlocks.get(id);
      if (at !== undefined) {
        const existing = rows[at].event;
        rows[at] = {
          ...rows[at],
          event: { ...existing, text: (existing.text ?? "") + (event.text ?? "") },
        };
      } else {
        openBlocks.set(id, rows.length);
        if (event.kind === "tool_call") callIndex.set(id, rows.length);
        rows.push({ event: { ...event }, key: rowKey(event, rows.length) });
      }
      continue;
    }

    if (event.phase === "completed" && id && openBlocks.has(id)) {
      const at = openBlocks.get(id)!;
      // The completed payload is authoritative; the accumulated text is dropped.
      rows[at] = { ...rows[at], event: { ...event } };
      openBlocks.delete(id);
      continue;
    }

    // `started` with no text would render an empty row; the first delta or the
    // completed event creates it instead. Tool calls are the exception: a call
    // that has started is worth showing immediately, since the user wants to
    // see that the agent is doing something.
    if (event.phase === "started" && !event.text && event.kind !== "tool_call") {
      continue;
    }

    if (id) {
      openBlocks.set(id, rows.length);
      if (event.kind === "tool_call") callIndex.set(id, rows.length);
    }
    rows.push({ event, key: rowKey(event, rows.length) });
  }

  return rows;
}

function rowKey(event: StreamEvent, index: number): string {
  return `${event.seq}-${event.streamId ?? index}`;
}

/**
 * Assemble rows into the tree their `span` fields describe.
 *
 * Single pass, then a bottom-up rollup. An event naming a trajectory that was
 * never opened is attached at the root rather than dropped: the protocol requires
 * that, and the case it protects is the one where structure is least reliable —
 * a link that died mid-delegation, or events arriving out of order. Losing work
 * there would hide exactly the failure the user is looking for.
 */
/**
 * Nodes from the previous build, keyed by the row object they wrapped.
 *
 * `buildRows` deliberately returns the *same* row object for a row whose content
 * did not change, which is what lets the memoized renderer skip settled history
 * during a stream. Allocating a fresh node per row would throw that away one layer
 * up: every node identity would change on every token, and the memo comparison
 * would never succeed — so a fan-out would re-render its whole subtree at token
 * rate. A `WeakMap` keyed on the row means a node survives exactly as long as the
 * row it describes, with no cache to invalidate.
 */
const NODE_CACHE = new WeakMap<TimelineRow, TimelineNode>();

/** Element-wise identity, to decide whether a cached node can be reused. */
function sameChildren(a: TimelineNode[], b: TimelineNode[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

export function buildTree(rows: TimelineRow[]): TimelineNode[] {
  const roots: TimelineNode[] = [];
  const byTrajectory = new Map<string, TimelineNode>();
  /** Children of a trajectory not yet seen, kept until it arrives. */
  const orphans = new Map<string, TimelineNode[]>();
  /** Nodes built this pass, so reuse is decided after children are known. */
  const built: TimelineNode[] = [];

  for (const row of rows) {
    const trajectory =
      row.event.kind === "subagent" ? row.event.streamId : undefined;

    const node: TimelineNode = {
      row,
      trajectory,
      children: [],
      rollup: EMPTY_ROLLUP,
      key: row.key,
    };
    built.push(node);

    if (trajectory) {
      byTrajectory.set(trajectory, node);
      // Children may have been parsed before their parent's `started` was seen.
      const waiting = orphans.get(trajectory);
      if (waiting) {
        node.children.push(...waiting);
        orphans.delete(trajectory);
      }
    }

    const parentId = row.event.span;
    if (!parentId) {
      roots.push(node);
      continue;
    }
    const parent = byTrajectory.get(parentId);
    if (parent) {
      parent.children.push(node);
    } else {
      // Held rather than rooted: the parent usually arrives, and rooting
      // immediately would leave a delegation's work duplicated at two depths.
      const waiting = orphans.get(parentId);
      if (waiting) waiting.push(node);
      else orphans.set(parentId, [node]);
    }
  }

  // A parent that never arrived. Surfaced at the root so nothing is swallowed.
  for (const stranded of orphans.values()) roots.push(...stranded);

  for (const node of roots) computeRollup(node);
  return roots;
}

/**
 * Bottom-up aggregate, and the node's own resolved status.
 *
 * Recursive rather than iterative because the depth is the *user's* nesting — a
 * handful of levels in practice, bounded by how many times an agent delegates
 * within one turn. A stack would buy nothing and obscure the shape.
 */
function computeRollup(node: TimelineNode): Rollup {
  let events = 0;
  let trajectories = 0;
  let tools = 0;
  let failures = 0;
  let depth = 0;
  let input = 0;
  let output = 0;
  let sawTokens = false;

  for (const child of node.children) {
    const sub = computeRollup(child);
    events += sub.events + 1;
    trajectories += sub.trajectories + (child.trajectory ? 1 : 0);
    tools += sub.tools + (isCompletedTool(child.row) ? 1 : 0);
    failures += sub.failures + (rowFailed(child.row) ? 1 : 0);
    depth = Math.max(depth, sub.depth + 1);
    if (sub.tokens) {
      input += sub.tokens.input;
      output += sub.tokens.output;
      sawTokens = true;
    }
    const own = ownTokens(child.row.event);
    if (own) {
      input += own.input;
      output += own.output;
      sawTokens = true;
    }
  }

  const status = node.trajectory ? trajectoryStatus(node, failures) : undefined;
  node.status = status;
  node.rollup = {
    events,
    trajectories,
    tools,
    failures,
    depth,
    tokens: sawTokens ? { input, output } : undefined,
    // Only for a closed trajectory. A running one would need a clock, and a
    // duration that ticks re-renders the row at 1 Hz forever — the renderer shows
    // "running" instead, which is the fact that matters while it is still true.
    durationMs:
      node.row.closedAtMs !== undefined
        ? Math.max(0, node.row.closedAtMs - node.row.event.createdAtMs)
        : undefined,
  };
  return node.rollup;
}

/**
 * A trajectory's status, from its own phase and its subtree.
 *
 * A descendant failure escalates the parent to `failed` even when the parent's
 * own result came back clean, because a collapsed card is the only thing the user
 * will see: a green row hiding a failed delegate is worse than no row at all. The
 * distinction between a *reported* failure and an *inherited* one lives in the
 * rollup counts, which the card renders separately.
 */
function trajectoryStatus(node: TimelineNode, failures: number): TrajectoryStatus {
  const event = node.row.event;
  if (event.phase !== "completed") return "running";
  const closedBy = (event.data as { closedBy?: string } | undefined)?.closedBy;
  if (closedBy === "server") return "abandoned";
  if (event.isError) return "failed";
  return failures > 0 ? "failed" : "done";
}

function isCompletedTool(row: TimelineRow): boolean {
  return row.event.kind === "tool_call" && Boolean(row.result);
}

function rowFailed(row: TimelineRow): boolean {
  if (row.event.isError || row.result?.isError) return true;
  return row.event.kind === "error";
}

/** Token usage an event reports for itself. Spellings differ per agent. */
function ownTokens(event: StreamEvent): { input: number; output: number } | null {
  if (event.kind !== "usage") return null;
  const usage = event.data as Record<string, unknown> | undefined;
  if (!usage) return null;
  const input = numeric(usage, ["inputTokens", "input_tokens", "prompt_tokens"]);
  const output = numeric(usage, ["outputTokens", "output_tokens", "completion_tokens"]);
  if (input === undefined && output === undefined) return null;
  return { input: input ?? 0, output: output ?? 0 };
}

function numeric(source: Record<string, unknown>, keys: string[]): number | undefined {
  for (const key of keys) {
    const value = source[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
  }
  return undefined;
}

/**
 * The path from the root to a trajectory, for a breadcrumb.
 *
 * Returns `null` when the id is not in the tree, which is what tells focus mode
 * that the node it was pointed at is gone — a turn cleared, a session switched —
 * so it can fall back to the root rather than render an empty pane.
 */
export function pathTo(
  nodes: TimelineNode[],
  trajectory: string,
): TimelineNode[] | null {
  for (const node of nodes) {
    if (node.trajectory === trajectory) return [node];
    const below = pathTo(node.children, trajectory);
    if (below) return [node, ...below];
  }
  return null;
}

/** Every trajectory in the tree, depth-first, for an outline. */
export function flattenTrajectories(
  nodes: TimelineNode[],
  depth = 0,
): { node: TimelineNode; depth: number }[] {
  const out: { node: TimelineNode; depth: number }[] = [];
  for (const node of nodes) {
    if (node.trajectory) {
      out.push({ node, depth });
      out.push(...flattenTrajectories(node.children, depth + 1));
    } else {
      out.push(...flattenTrajectories(node.children, depth));
    }
  }
  return out;
}
