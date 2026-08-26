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
  /**
   * A `subagent` close that arrived for a row which is not itself a `subagent`.
   *
   * A promoted `tool_call` (`docs/session-protocol.md` § Promotion) is normally
   * closed by its own `tool_result`. When the work is abandoned there is no result,
   * and the server's closing event is all there is — but folding that into `event`
   * would rewrite a tool call into a subagent, losing the tool's name, arguments,
   * and card. Kept beside it so the row stays a tool call that is known to have
   * been abandoned.
   */
  trajectoryClose?: StreamEvent;
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
  /**
   * streamId -> index in `rows`, for every id ever seen. Unlike `openBlocks`,
   * nothing is ever removed: a promoted trajectory's server-issued close arrives
   * long after its `tool_call` completed and was dropped from `openBlocks`, and
   * without a durable index that close cannot find the row it belongs to. It then
   * became a second root row, which is how an abandoned delegation showed up twice.
   */
  const rowIndex = new Map<string, number>();

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
      // A `subagent` event addressing a row that is not one: the host of a promoted
      // trajectory. Its own lifecycle is its `tool_result`, so this is recorded
      // beside the row rather than folded into it — merging would rewrite the tool
      // call into a subagent and discard its name, arguments, and card.
      const host = rowIndex.get(id);
      if (host !== undefined && rows[host].event.kind !== "subagent") {
        if (event.phase === "completed") {
          rows[host] = {
            ...rows[host],
            trajectoryClose: event,
            closedAtMs: rows[host].closedAtMs ?? event.createdAtMs,
          };
        } else {
          // Progress on a promoted host. `data` replaces because a progress
          // summary is a snapshot (§ Promotion), and text is left alone: the
          // host's own text is its tool arguments, which this does not describe.
          rows[host] = {
            ...rows[host],
            event: {
              ...rows[host].event,
              data: event.data ?? rows[host].event.data,
            },
          };
        }
        continue;
      }

      const at = openBlocks.get(id);
      if (at !== undefined) {
        const existing = rows[at].event;
        rows[at] = {
          ...rows[at],
          closedAtMs:
            event.phase === "completed" ? event.createdAtMs : rows[at].closedAtMs,
          event: mergedEvent(event, {
            ...existing,
            phase: event.phase,
            // A later name is better informed: the agent's type is only known
            // once its streamed brief parses.
            name: event.name ?? existing.name,
            text: event.text || existing.text,
            data: event.data ?? existing.data,
            isError: event.isError || existing.isError,
            span: existing.span ?? event.span,
          }),
        };
        continue;
      }
      openBlocks.set(id, rows.length);
      rowIndex.set(id, rows.length);
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
        rowIndex.set(id, rows.length);
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
      rowIndex.set(id, rows.length);
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
 * Merged events from previous builds, keyed by the arriving event that produced them.
 *
 * A trajectory's row folds its phases into a *new* event object, so unlike every
 * other row its event identity changed on each build — and identity is what both the
 * node cache and the renderer's comparator compare. The consequence was the opposite
 * of the intent: settled delegations, the rows most worth skipping, were the only
 * ones that could never be skipped, so a session with many of them re-rendered all of
 * them per token.
 *
 * Keyed on the arriving event because it is unique to one phase of one block, and the
 * fold is a pure function of that event and the prefix before it — which, while a
 * turn streams, does not change. `WeakMap`, so an entry lives exactly as long as the
 * event and nothing has to be invalidated.
 */
const MERGE_CACHE = new WeakMap<StreamEvent, StreamEvent>();

/** The previous build's object for this fold, when it produced an equal one. */
function mergedEvent(arriving: StreamEvent, merged: StreamEvent): StreamEvent {
  const cached = MERGE_CACHE.get(arriving);
  if (cached && sameEvent(cached, merged)) return cached;
  MERGE_CACHE.set(arriving, merged);
  return merged;
}

/** Field-wise comparison of the fields the fold above can change. */
function sameEvent(a: StreamEvent, b: StreamEvent): boolean {
  return (
    a.phase === b.phase &&
    a.name === b.name &&
    a.text === b.text &&
    a.data === b.data &&
    a.isError === b.isError &&
    a.span === b.span &&
    a.seq === b.seq &&
    a.kind === b.kind &&
    a.streamId === b.streamId
  );
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
 * Nodes from the previous build, keyed by the **event** their row wrapped.
 *
 * The memoized renderer compares nodes by identity, so a fresh node per build makes
 * every comparison fail and a fan-out re-renders its whole subtree at token rate.
 * Reuse therefore has to survive across builds.
 *
 * Keyed on the event and not the row, which is the distinction that makes this work
 * at all: `buildRows` reuses the *event* object of an unchanged row, but always
 * allocates a new row to hold it. A cache keyed on the row can never hit — which is
 * why the one this replaces sat declared and unread, describing a guarantee
 * `buildRows` does not make. `lib/timeline-rows.test.ts` pins the guarantee it does
 * make, and `MemoizedNode`'s comparator already depends on it.
 *
 * A `WeakMap` means an entry lives exactly as long as the event it describes, so
 * there is no cache to invalidate and no growth across a long session.
 */
const NODE_CACHE = new WeakMap<StreamEvent, TimelineNode>();

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
  /** Every node this pass built, so the reachability repair below can find them. */
  const built: TimelineNode[] = [];

  // Promotion (`docs/session-protocol.md` § Promotion): a row whose id another row
  // names as its `span` is a trajectory, whatever kind of event opened it. Resolved
  // in its own pass because it is a property of the *whole* list — a `tool_call`
  // becomes a container only once a child referencing it has arrived, which is
  // always after the call itself. One extra linear scan over rows already in memory,
  // which is what buys the guarantee that no delegation renders flat.
  const referenced = new Set<string>();
  for (const row of rows) {
    if (row.event.span) referenced.add(row.event.span);
    // A `subagent` event addressing a row that is not one is the other way a host is
    // named: not "my work happened in you" but "you are a delegation, and here is
    // its outcome". That is the only signal a host reporting *no* interior ever
    // gets — nothing carries its id as a span, because nothing was forwarded — so
    // without this it stayed a plain tool call and the delegation was invisible.
    // Measured against `claude`'s `Agent`, which forwards no interior at all.
    if (row.trajectoryClose?.streamId) referenced.add(row.trajectoryClose.streamId);
  }

  for (const row of rows) {
    const id = row.event.streamId;
    const trajectory =
      id && (row.event.kind === "subagent" || referenced.has(id)) ? id : undefined;

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
    // A row naming itself is rooted, not parented. `claude` reports a nested
    // delegation's interior events with the child's own id in both fields, and a
    // node held as its own child is unreachable from the root — the subtree
    // vanishes, and `computeRollup` would recurse on it forever.
    if (!parentId || parentId === trajectory) {
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

  // Anything a cycle made unreachable. Two rows naming each other attach to one
  // another and to nothing else, so neither is a root and neither is reachable —
  // the whole pair disappears from the timeline. That is strictly worse than the
  // recursion the rollup guards against, because it is silent: work that arrived
  // is not rendered and nothing reports it missing. No single-pass attachment can
  // prevent it (the first waits as an orphan for the second, which then adopts it
  // and is adopted by it), so it is repaired here instead, where reachability is
  // decidable. Rooting the survivor keeps the events visible, which is the same
  // trade the protocol already makes for an unknown `span`.
  const reachable = new Set<TimelineNode>();
  const stack = [...roots];
  while (stack.length) {
    const node = stack.pop()!;
    if (reachable.has(node)) continue;
    reachable.add(node);
    stack.push(...node.children);
  }
  for (const node of built) {
    if (!reachable.has(node)) {
      roots.push(node);
      // Marked before descending, so a sibling inside the same cycle is not rooted
      // a second time as this one's descendant.
      const inner = [node];
      while (inner.length) {
        const seen = inner.pop()!;
        if (reachable.has(seen)) continue;
        reachable.add(seen);
        inner.push(...seen.children);
      }
    }
  }

  for (const node of roots) computeRollup(node, new Set());

  // Bottom-up, because a node is only interchangeable with its cached twin once
  // its children are — and a parent canonicalized before its children would keep
  // pointing at the nodes they replaced.
  return roots.map((root) => canonicalize(root));
}

/**
 * Reuse the node from the previous build when nothing about this one differs.
 *
 * This is what lets the memoized renderer skip settled history while a turn
 * streams. `buildRows` already returns the same row object for an unchanged row,
 * but the node wrapping it was allocated fresh on every build — so every memo
 * comparison one layer up failed anyway, and a fan-out re-rendered its whole
 * subtree at token rate. Children are compared by identity, which after this pass
 * means an unchanged subtree compares in one pointer check per child.
 */
function canonicalize(
  node: TimelineNode,
  seen: Set<TimelineNode> = new Set(),
): TimelineNode {
  // Same cycle hazard as `computeRollup`, and the same guard. A revisited node is
  // returned as-is rather than re-canonicalized.
  if (seen.has(node)) return node;
  seen.add(node);

  for (let i = 0; i < node.children.length; i += 1) {
    node.children[i] = canonicalize(node.children[i], seen);
  }

  const cached = NODE_CACHE.get(node.row.event);
  if (
    cached &&
    cached.trajectory === node.trajectory &&
    cached.status === node.status &&
    // The rest of the row. An event can be shared while what surrounds it changed:
    // a tool call gains a result, a trajectory gains a close. Compared by identity
    // for the same reason as the event itself — `buildRows` reuses these objects too.
    cached.row.result === node.row.result &&
    cached.row.trajectoryClose === node.row.trajectoryClose &&
    cached.row.closedAtMs === node.row.closedAtMs &&
    sameChildren(cached.children, node.children)
  ) {
    return cached;
  }

  NODE_CACHE.set(node.row.event, node);
  return node;
}

/**
 * Bottom-up aggregate, and the node's own resolved status.
 *
 * Recursive rather than iterative because the depth is the *user's* nesting — a
 * handful of levels in practice, bounded by how many times an agent delegates
 * within one turn. A stack would buy nothing and obscure the shape.
 *
 * `seen` is not a formality. `span` is now honoured from any event rather than only
 * a declared `subagent` (§ Promotion), so more ids reach this code, and two rows
 * naming each other build a cycle no single-pass attachment can detect: the first
 * waits in `orphans` for the second, which then adopts it *and* is adopted by it.
 * Unbounded recursion in a render path is a frozen tab, so the tree is walked with
 * a guard rather than the input being trusted to be acyclic. A revisited node
 * contributes nothing, which keeps every aggregate a count of distinct work.
 */
function computeRollup(node: TimelineNode, seen: Set<TimelineNode>): Rollup {
  if (seen.has(node)) return EMPTY_ROLLUP;
  seen.add(node);

  let events = 0;
  let trajectories = 0;
  let tools = 0;
  let failures = 0;
  let depth = 0;
  let input = 0;
  let output = 0;
  let sawTokens = false;

  for (const child of node.children) {
    const sub = computeRollup(child, seen);
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

  // A promoted trajectory (§ Promotion) is a tool call, and a tool call's
  // `completed` phase means its *arguments* finished streaming — the delegated work
  // has not started. Reading the phase here would report every delegation as
  // finished the instant its input parsed. Its close is the `tool_result` sharing
  // its id, or, when the work was abandoned, the server's own closing event.
  if (event.kind !== "subagent") {
    const close = node.row.trajectoryClose;
    if (close) {
      if (closedByServer(close)) return "abandoned";
      return close.isError || failures > 0 ? "failed" : "done";
    }
    const result = node.row.result;
    if (!result || result.phase !== "completed") return "running";
    return result.isError || failures > 0 ? "failed" : "done";
  }

  if (event.phase !== "completed") return "running";
  if (closedByServer(event)) return "abandoned";
  if (event.isError) return "failed";
  return failures > 0 ? "failed" : "done";
}

function isCompletedTool(row: TimelineRow): boolean {
  return row.event.kind === "tool_call" && Boolean(row.result);
}

function rowFailed(row: TimelineRow): boolean {
  // A trajectory the *server* closed is `abandoned`, and abandoned is not a
  // failure: nobody reported an outcome, which is a different fact from work that
  // reported an error. The server sets `isError` on its closing event so the row
  // reads as unresolved rather than clean — but counting that as a descendant
  // failure escalated every ancestor to `failed`, so a `Workflow` whose own
  // `tool_result` succeeded was badged red because one delegate was still running
  // when the turn ended. That reports an interrupted turn as a broken one, which is
  // exactly the distinction `abandoned` exists to preserve.
  if (closedByServer(row.event) || closedByServer(row.trajectoryClose)) return false;
  if (row.event.isError || row.result?.isError) return true;
  return row.event.kind === "error";
}

/** Whether an event is the server's own close of a trajectory an adapter left open. */
function closedByServer(event: StreamEvent | undefined): boolean {
  if (!event) return false;
  return (event.data as { closedBy?: string } | undefined)?.closedBy === "server";
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

/**
 * A trajectory's display name, never the vendor's tool name.
 *
 * One definition, because two is what this had and the copies could disagree. The card
 * and the sibling-comparison strip each computed it, so a change to either would label
 * one node two ways — and the strip's `aria-label` would stop matching the card's region
 * name, which nothing type-checks and no test asserts.
 *
 * # Why this is the only producer, and what that took
 *
 * It was not always. `claude`'s adapter used to send the literal `"Subagent"` as the
 * `name` on the wire whenever a delegation's brief had not finished streaming — which at
 * `started` is the usual case, since the brief arrives as partial JSON. So the string on
 * screen had two producers, and renaming this one would only have changed which of them
 * put it there.
 *
 * The adapter's half was the more serious one, and it was a *data* defect rather than a
 * display one. `name` means "what this delegation is called", so `null` says nobody knows
 * yet while a literal asserts a name the adapter cannot support — and that guess is
 * journaled. `history::consolidate` keeps the best name any phase gave a block, so a
 * later `delta` carrying the real brief normally wins; but when the brief never finishes
 * streaming (the delegation fails early, or the link dies mid-input) the placeholder
 * becomes the trajectory's durable name in `history.db`.
 *
 * `_task_label` returns `None` as of `c8783b3` (its tests widened in `438a808`), which
 * is what makes this the sole
 * producer and the rename above safe. **That is a dependency of this code, not a
 * historical note**: move the rename to a tree without that commit — a cherry-pick, an
 * older branch — and the two-label state returns, silently, until a card renders. The
 * commit is named here so the check is a `git log` rather than a memory.
 *
 * **The bound on what that fixed is worth stating exactly, because it sounds
 * better than it is: rows already journaled with the literal are permanently
 * indistinguishable from a genuine name.** There was nothing to migrate not because the
 * data is fine but because nothing can tell the two apart — the fix stops new guesses and
 * cannot undo old ones. So a historical `"Subagent"` row is not evidence that an agent
 * named it that, and any analysis over archived names has to treat those as unknowable
 * rather than as data.
 *
 * The **ordering** is the part worth keeping now that both halves have landed, because it
 * is the trap anyone repeating this shape will walk into. The two fixes were not
 * interchangeable: renaming *this* while the adapter still sent a literal would have given
 * one condition two labels — the adapter's string for trajectories it named, this one for
 * those it did not — which is worse than a single imprecise label, and invisible until a
 * card renders. The general rule: when two layers both supply a default for one field,
 * the layer that *writes data* stops first and the layer that *renders* changes second.
 */
export function trajectoryName(node: TimelineNode): string {
  return node.row.event.name?.trim() || UNNAMED_TRAJECTORY;
}

/**
 * What a delegation is called when nobody said.
 *
 * "Unnamed" rather than "Subagent", for two reasons that point the same way.
 *
 * **It states what is true.** The fallback fires exactly when no name arrived, and
 * "Subagent" restates a category the chrome already carries — the branch icon, the
 * card, the word "delegation" in the region label all say that much. "Unnamed" says the
 * thing the row actually knows: the agent did not report what this is. That is the same
 * rule as the `≥` on a bounded duration and as the "outlived its turn" wording — a
 * surface may not claim more than it has.
 *
 * **It closes a collision class rather than guarding each instance.** "Subagent
 * delegation" *contains* "agent delegation", and `Agent` is a tool `claude` ships, so
 * every `{ name: "<tool> delegation" }` locator was ambiguous with the fallback by
 * construction. That cost two real failures and five `exact: true` fixes. No tool name
 * is a substring of "Unnamed delegation", so a future locator is safe without anyone
 * remembering the rule. The `exact: true` guards stay regardless: they are correct on
 * their own terms, and cost nothing.
 *
 * Deliberately a named constant rather than an inline literal, because the ordering
 * hazard above is a property of *this string's relationship to the adapter's*, and a
 * grep for it should land somewhere that explains that.
 */
const UNNAMED_TRAJECTORY = "Unnamed";

/**
 * What a delegation says about itself: its closing summary, else its latest progress.
 *
 * The protocol records both — `task_notification` carries the delegate's own summary,
 * and `data.progress` is a snapshot the vendor re-sends — and the card rendered
 * neither, so a delegate's account of its work was stored and never shown. That is the
 * one piece of *content* a collapsed row can carry: without it the row is a status and
 * a set of counts, which says that work happened and nothing about what came of it.
 * It matters more now that a late outcome can arrive on a turn of the agent's own,
 * because then this summary is the whole of what a user learns about work that
 * finished after the turn that dispatched it.
 *
 * Two rules, and both are why this is a function here rather than an expression in the
 * renderer — `lib/timeline-tree.test.ts` can pin them, and a component in a `node`
 * test environment cannot be rendered at all:
 *
 * 1. **A promoted host's `text` is not a summary.** It is the tool call's serialized
 *    arguments (a `Workflow` script, a JSON blob), so reading `text` uniformly would
 *    put a wall of JSON where a sentence belongs. Its outcome instead arrives as the
 *    `subagent` close kept *beside* the row — `trajectoryClose`, which exists exactly
 *    so a promotion does not overwrite the call.
 *
 *    **And not its `tool_result` either**, however reasonable that looks: a promoted
 *    host's lifecycle *is* its result (`docs/session-protocol.md` § Promotion), so
 *    reaching for `row.result` here is the obvious next step for anyone extending this.
 *    It is wrong because a result is the tool's output, not the delegation's account of
 *    itself — and it is load-bearing beyond taste. A card's text is matched by
 *    `not.toContainText(...)` assertions in `e2e/delegation.spec.ts`, so a status word
 *    appearing inside a summary fails them as though the *status* were wrong. The real
 *    fixture already contains the ammunition: `pr-host-d`'s result reads
 *    "dispatched; running in background", and the `promote` scenario asserts that host
 *    is not `failed` and that a server-closed trajectory is not `running`. The test
 *    below pins this so the rule fails here, next to the reason, rather than three
 *    layers away as a status bug.
 * 2. **A close outranks progress.** Progress is a snapshot of work in flight; once the
 *    delegation reports an outcome, the snapshot describes a state that has been
 *    superseded. Preferring the fresher-arriving field would leave a finished
 *    delegation captioned with its own last guess.
 */
export function trajectoryReport(node: TimelineNode): string | undefined {
  const { row } = node;
  const declared = row.event.kind === "subagent";

  // A declared trajectory's own text is its summary. A promoted host's is its
  // arguments, so its summary can only come from the close held beside the row.
  const summary = declared ? row.event.text : row.trajectoryClose?.text;
  if (summary?.trim()) return summary.trim();

  const progress = (row.event.data as { progress?: unknown } | undefined)?.progress;
  if (typeof progress === "string" && progress.trim()) return progress.trim();
  return undefined;
}

/**
 * Every trajectory in the tree, depth-first, for an outline.
 *
 * `seen` carries the same guard as `computeRollup` and `canonicalize`, and for the
 * same reason: `buildTree` repairs a `span` cycle by *reachability*, which roots the
 * survivor without breaking the child pointers — so two nodes holding each other are
 * reachable and still cyclic. Every other walk over `children` already guarded; this
 * one did not, and it is the one an overview iterates on every build. Unbounded
 * recursion in a render path is a frozen tab.
 */
export function flattenTrajectories(
  nodes: TimelineNode[],
  depth = 0,
  seen: Set<TimelineNode> = new Set(),
): { node: TimelineNode; depth: number }[] {
  const out: { node: TimelineNode; depth: number }[] = [];
  for (const node of nodes) {
    if (seen.has(node)) continue;
    seen.add(node);
    if (node.trajectory) {
      out.push({ node, depth });
      out.push(...flattenTrajectories(node.children, depth + 1, seen));
    } else {
      out.push(...flattenTrajectories(node.children, depth, seen));
    }
  }
  return out;
}
