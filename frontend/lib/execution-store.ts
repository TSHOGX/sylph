import { segment } from "./api";
import type { StreamEvent } from "./api-types";

export interface ExecutionNode {
  id: string;
  removed?: boolean;
  structureError?: string | null;
  parentId: string;
  turnId: string;
  ordinal: number;
  orderTie: number;
  revision: number;
  bodyRevision: number;
  event: StreamEvent;
  trajectory?: StreamEvent | null;
  children: number;
  tools: number;
  reads: number;
  commands: number;
  thinking: number;
  failed: number;
  running: number;
  waiting: number;
  unfinished?: number;
  activity: string;
}
interface Page {
  parentNode?: ExecutionNode;
  parent?: string;
  targetId?: string | null;
  nodes: ExecutionNode[];
  snapshot: number;
  nextCursor: string | null;
  hasMore: boolean;
}
interface Branch {
  ids: string[];
  cursor?: string;
  changes?: string;
  controller?: AbortController;
  timer?: ReturnType<typeof setTimeout>;
  open: boolean;
  initialized: boolean;
  earlierRequested?: boolean;
  detached?: boolean;
}
export interface VisibleNode {
  id: string;
  depth: number;
}
const MAX_BRANCHES = 16;
const MAX_CHILDREN = 160;

function compareOrder(a: ExecutionNode, b: ExecutionNode) {
  return (
    a.ordinal - b.ordinal || a.orderTie - b.orderTie ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  );
}

/** Entity updates notify only that row. The visible list changes only on insertion
 * or disclosure, not on token updates. The cache and subscriptions are bounded. */
export class ExecutionStore {
  readonly nodes = new Map<string, ExecutionNode>();
  readonly branches = new Map<string, Branch>();
  private listeners = new Map<string, Set<() => void>>();
  private visible: VisibleNode[] = [];
  error: string | null = null;
  ready = false;
  active = false;
  rootParent = "";
  targetId: string | null = null;
  private seekController?: AbortController;
  constructor(readonly sessionId: string) {}
  subscribe = (id: string, fn: () => void) => {
    const listeners = this.listeners.get(id) ?? new Set();
    listeners.add(fn);
    this.listeners.set(id, listeners);
    return () => {
      listeners.delete(fn);
      if (!listeners.size && this.listeners.get(id) === listeners)
        this.listeners.delete(id);
    };
  };
  private emit(id: string) {
    this.listeners.get(id)?.forEach((fn) => fn());
  }
  snapshot = () => this.visible;
  private rebuild() {
    const next: VisibleNode[] = [];
    const seen = new Set<string>();
    const visit = (parent: string, depth: number) => {
      const branch = this.branches.get(parent);
      if (!branch?.open) return;
      for (const id of branch.ids) {
        if (seen.has(id)) continue;
        seen.add(id);
        next.push({ id, depth });
        visit(id, depth + 1);
      }
    };
    visit(this.rootParent, 0);
    this.visible = next;
    this.emit("");
  }
  merge(parent: string, page: Page, changes = false) {
    const branch = this.branches.get(parent);
    if (!branch) return;
    if (page.parentNode) {
      const previous = this.nodes.get(page.parentNode.id);
      if (!previous || previous.revision < page.parentNode.revision) {
        this.nodes.set(page.parentNode.id, page.parentNode);
        this.emit(page.parentNode.id);
      }
    }
    const oldCursor = branch.cursor;
    let structural = !this.ready;
    for (const incoming of page.nodes) {
      let node = incoming;
      const previous = this.nodes.get(node.id);
      if (node.removed) {
        if (
          previous &&
          previous.revision > node.revision &&
          previous.parentId === parent
        )
          continue;
        const index = branch.ids.indexOf(node.id);
        if (index >= 0) {
          branch.ids.splice(index, 1);
          structural = true;
        }
        this.evict(node.id);
        this.emit(node.id);
        continue;
      }
      if (
        previous &&
        previous.revision >= node.revision &&
        branch.ids.includes(node.id)
      )
        continue;
      if (
        previous &&
        previous.revision > node.revision &&
        previous.parentId !== parent
      )
        continue;
      if (previous && previous.revision > node.revision) node = previous;
      const existing = branch.ids.includes(node.id);
      const reordered =
        existing &&
        previous &&
        (previous.ordinal !== node.ordinal || previous.orderTie !== node.orderTie);
      // Corrections can change order without changing identity. Reinsert that one
      // address; sorting the window would also touch every unchanged row.
      if (reordered) branch.ids.splice(branch.ids.indexOf(node.id), 1);
      if (!existing || reordered) {
        const first = this.nodes.get(branch.ids[0]);
        const last = this.nodes.get(branch.ids[branch.ids.length - 1]);
        // An update to unloaded older history is not permission to load that window.
        if (
          !existing &&
          changes &&
          ((branch.cursor && first && compareOrder(node, first) < 0) ||
            (branch.detached && (!last || compareOrder(node, last) > 0)))
        )
          continue;
        this.nodes.set(node.id, node);
        let lo = 0,
          hi = branch.ids.length;
        while (lo < hi) {
          const mid = (lo + hi) >>> 1,
            other = this.nodes.get(branch.ids[mid])!;
          if (compareOrder(other, node) < 0)
            lo = mid + 1;
          else hi = mid;
        }
        branch.ids.splice(lo, 0, node.id);
        structural = true;
      }
      this.nodes.set(node.id, node);
      this.emit(node.id);
    }
    if (branch.ids.length > MAX_CHILDREN) {
      if (!changes && branch.initialized) branch.detached = true;
      for (const id of branch.ids.splice(
        !changes && branch.initialized ? MAX_CHILDREN : 0,
        branch.ids.length - MAX_CHILDREN,
      ))
        this.evict(id);
      if (changes) {
        // The previous server cursor may precede rows just evicted locally (or
        // be absent for a formerly complete window). Resume at the kept edge.
        const first = this.nodes.get(branch.ids[0])!;
        branch.cursor = `${first.ordinal}:${first.orderTie}:${first.id}`;
      }
      structural = true;
    }
    if (changes) branch.changes = page.nextCursor ?? `${page.snapshot + 1}:`;
    else {
      branch.cursor = page.hasMore ? (page.nextCursor ?? undefined) : undefined;
      branch.changes ??= `${page.snapshot + 1}:`;
    }
    branch.initialized = true;
    this.ready = true;
    this.error = null;
    if (structural || oldCursor !== branch.cursor) this.rebuild();
    if (parent) this.emit(parent);
  }
  private evict(id: string) {
    // A move can reach its destination before its former-parent removal. Only
    // the last loaded owner can release an entity and its child window.
    if (
      id === this.rootParent ||
      [...this.branches.values()].some((b) => b.ids.includes(id))
    )
      return;
    this.releaseBranch(id);
    this.nodes.delete(id);
  }
  private releaseBranch(id: string) {
    const branch = this.branches.get(id);
    branch?.controller?.abort();
    clearTimeout(branch?.timer);
    this.branches.delete(id);
    if (branch) for (const child of branch.ids) this.evict(child);
  }
  private async read(parent: string, changes: boolean, earlier = false) {
    const branch = this.branches.get(parent);
    if (!branch?.open || branch.controller) return;
    const controller = new AbortController();
    branch.controller = controller;
    if (earlier) branch.earlierRequested = false;
    let delay =
      this.active &&
      (parent === this.rootParent || (this.nodes.get(parent)?.running ?? 0) > 0)
        ? 100
        : 500;
    try {
      const params = new URLSearchParams({ parent });
      const cursor = changes
        ? branch.changes
        : earlier
          ? branch.cursor
          : undefined;
      if (cursor) params.set("cursor", cursor);
      if (changes) params.set("changes", "true");
      const response = await fetch(
        `/api/v1/sessions/${segment(this.sessionId)}/nodes?${params}`,
        {
          signal: controller.signal,
          credentials: "include",
          cache: "no-store",
        },
      );
      if (!response.ok)
        throw new Error(`Could not read timeline (${response.status})`);
      const page = (await response.json()) as Page;
      if (controller.signal.aborted || this.branches.get(parent) !== branch)
        return;
      this.merge(parent, page, changes);
      if (changes && page.hasMore) delay = 0;
    } catch (error) {
      delay = 2000;
      if (!controller.signal.aborted) {
        this.error = error instanceof Error ? error.message : String(error);
        this.rebuild();
      }
    } finally {
      if (branch.controller === controller) branch.controller = undefined;
      if (branch.open && !controller.signal.aborted)
        branch.timer = setTimeout(
          () =>
            void this.read(
              parent,
              branch.initialized && !branch.earlierRequested,
              Boolean(branch.earlierRequested),
            ),
          branch.earlierRequested ? 0 : delay,
        );
    }
  }
  open(parent: string) {
    if (!this.branches.has(parent) && this.branches.size >= MAX_BRANCHES) {
      const candidate = [...this.branches].find(([id, b]) => id && !b.open);
      // Discard the closed window, not the summary its parent still renders.
      if (candidate) this.releaseBranch(candidate[0]);
      else {
        this.error = "Collapse a branch before opening more.";
        this.rebuild();
        return;
      }
    }
    const branch = this.branches.get(parent) ?? {
      ids: [],
      open: true,
      initialized: false,
    };
    branch.open = true;
    clearTimeout(branch.timer);
    this.branches.set(parent, branch);
    this.rebuild();
    this.emit(parent);
    void this.read(parent, branch.initialized);
  }
  close(parent: string) {
    const branch = this.branches.get(parent);
    if (!branch) return;
    branch.open = false;
    branch.controller?.abort();
    branch.controller = undefined;
    clearTimeout(branch.timer);
    for (const child of branch.ids)
      if (this.branches.get(child)?.open) this.close(child);
    this.rebuild();
    this.emit(parent);
  }
  collapseAll() {
    for (const id of this.branches.keys())
      if (id !== this.rootParent) this.close(id);
  }
  earlier(parent = this.rootParent) {
    const b = this.branches.get(parent);
    if (b?.cursor) {
      b.earlierRequested = true;
      clearTimeout(b.timer);
      void this.read(parent, false, true);
    }
  }
  start() {
    this.open(this.rootParent);
  }
  focus(parent: string) {
    this.stop();
    this.nodes.clear();
    this.branches.clear();
    this.rootParent = parent;
    this.targetId = null;
    this.ready = false;
    this.open(parent);
  }
  async seek(seq: number) {
    this.seekController?.abort();
    const controller = new AbortController();
    this.seekController = controller;
    try {
      const response = await fetch(
        `/api/v1/sessions/${segment(this.sessionId)}/nodes?around=${seq}`,
        { signal: controller.signal, credentials: "include" },
      );
      if (!response.ok)
        throw new Error(`Could not locate search result (${response.status})`);
      const page = (await response.json()) as Page;
      if (controller.signal.aborted || this.seekController !== controller)
        return;
      if (typeof page.parent !== "string" || !page.targetId)
        throw new Error("Invalid search window");
      for (const branch of this.branches.values()) {
        branch.open = false;
        branch.controller?.abort();
        clearTimeout(branch.timer);
      }
      this.nodes.clear();
      this.branches.clear();
      this.rootParent = page.parent;
      this.targetId = page.targetId;
      this.branches.set(page.parent, {
        ids: [],
        open: true,
        initialized: false,
        detached: true,
      });
      this.merge(page.parent, page);
      void this.read(page.parent, true);
    } catch (error) {
      if (!controller.signal.aborted) {
        this.error = error instanceof Error ? error.message : String(error);
        this.rebuild();
      }
    }
  }
  stop() {
    this.seekController?.abort();
    for (const branch of this.branches.values()) {
      branch.open = false;
      branch.controller?.abort();
      branch.controller = undefined;
      clearTimeout(branch.timer);
    }
  }
}

/** Usage belongs to its reporting scope; callers never sum it over descendants. */
export function reportedUsage(data: unknown): string {
  const object = (value: unknown) =>
    typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : undefined;
  const envelope = object(data);
  // Codex live/native stores wrap cumulative usage; Claude reports a flat turn
  // usage object. These are vendor shapes, not alternative Sylph wire spellings.
  const total = object(envelope?.total) ?? object(envelope?.total_token_usage);
  const value = total ?? envelope;
  const input = value?.inputTokens ?? value?.input_tokens;
  const output = value?.outputTokens ?? value?.output_tokens;
  const count = (value: unknown, label: string) =>
    typeof value === "number" && Number.isSafeInteger(value) && value >= 0
      ? `${value} ${label} tokens`
      : "";
  const text = [count(input, "input"), count(output, "output")]
    .filter(Boolean)
    .join(" · ");
  return text
    ? `${total ? "Reported total: " : ""}${text}`
    : "Usage was not reported";
}
