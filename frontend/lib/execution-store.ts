import { segment } from "./api";
import { readResource } from "./reading-resource";
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
  readingRole?: "progress" | "answer" | "unknown";
  turn?: {
    id: string;
    status: string;
    origin: string;
    startedAtMs: number | null;
    completedAtMs: number | null;
    closeReason?: string | null;
    errorMessage?: string | null;
  };
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
interface EntityPool {
  nodes: Map<string, ExecutionNode>;
  views: Set<ExecutionStore>;
}
const pools = new Map<string, EntityPool>();
const MAX_BRANCHES = 16;
const MAX_CHILDREN = 160;

function compareOrder(a: ExecutionNode, b: ExecutionNode) {
  return (
    a.ordinal - b.ordinal ||
    a.orderTie - b.orderTie ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  );
}

/** Entity updates notify only that row. The visible list changes only on insertion
 * or disclosure, not on token updates. The cache and subscriptions are bounded. */
export class ExecutionStore {
  readonly nodes: Map<string, ExecutionNode>;
  private pool?: EntityPool;
  private started = false;
  get visibleReader() {
    return this.started;
  }
  readonly branches = new Map<string, Branch>();
  private listeners = new Map<string, Set<() => void>>();
  private visible: VisibleNode[] = [];
  error: string | null = null;
  ready = false;
  active = false;
  rootParent = "";
  targetId: string | null = null;
  private seekController?: AbortController;
  readonly disclosureChoices = new Map<string, boolean>();
  readonly observedStatus = new Map<string, string>();
  scrollTop = 0;
  readonly groupScroll = new Map<string, number>();
  constructor(
    readonly sessionId: string,
    readonly reading = false,
    readonly initialRoot = "",
  ) {
    this.rootParent = initialRoot;
    if (reading) {
      this.pool = pools.get(sessionId) ?? {
        nodes: new Map(),
        views: new Set(),
      };
      pools.set(sessionId, this.pool);
      this.pool.views.add(this);
    }
    this.nodes = this.pool?.nodes ?? new Map();
  }
  dispose() {
    this.stop();
    this.branches.clear();
    this.pool?.views.delete(this);
    this.prune();
    if (this.pool?.views.size === 0) pools.delete(this.sessionId);
  }
  private prune() {
    for (const id of this.nodes.keys()) this.evict(id);
  }
  /** Independent inner scrollers consume the same branch windows, not a second fetch. */
  rowsUnder(parent: string): VisibleNode[] {
    const rows: VisibleNode[] = [];
    const seen = new Set<string>();
    const visit = (id: string, depth: number) => {
      if (!this.branches.get(id)?.open) return;
      for (const child of this.branches.get(id)?.ids ?? []) {
        if (seen.has(child)) continue;
        seen.add(child);
        rows.push({ id: child, depth });
        if (this.nodes.get(child)?.event.kind !== "execution")
          visit(child, depth + 1);
      }
    };
    visit(parent, 0);
    return rows;
  }
  /** An intentional interaction inside work also pins its enclosing process.
   * Reading addresses differ from physical execution parents, so follow this
   * view's loaded windows rather than recomputing a synthetic work id. */
  chooseDisclosure(id: string, open: boolean) {
    this.disclosureChoices.set(id, open);
    const seen = new Set<string>([id]);
    let child = id;
    for (;;) {
      const parent = [...this.branches].find(
        ([key, branch]) => !seen.has(key) && branch.ids.includes(child),
      )?.[0];
      if (parent === undefined) break;
      seen.add(parent);
      if (this.nodes.get(parent)?.event.kind === "work")
        this.disclosureChoices.set(parent, true);
      child = parent;
    }
  }
  private endpoint(parent: string) {
    if (this.targetId) return "nodes";
    return this.reading &&
      (parent === this.initialRoot ||
        this.nodes.get(parent)?.event.kind === "work")
      ? "conversation"
      : "nodes";
  }
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
    const views = id && this.pool ? this.pool.views : [this];
    for (const view of views) view.listeners.get(id)?.forEach((fn) => fn());
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
        if (!this.reading || this.nodes.get(id)?.event.kind !== "execution")
          visit(id, depth + 1);
      }
    };
    visit(this.rootParent, 0);
    if (this.reading) {
      // Disclosure changes visibility, never narrative order. Root steering and
      // unclassified prose must stay between the process items surrounding them.
      next.sort((left, right) => {
        const a = this.nodes.get(left.id)!;
        const b = this.nodes.get(right.id)!;
        return (
          a.ordinal - b.ordinal ||
          a.orderTie - b.orderTie ||
          Number(b.event.kind === "work") - Number(a.event.kind === "work") ||
          (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
        );
      });
    }
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
        (previous.ordinal !== node.ordinal ||
          previous.orderTie !== node.orderTie);
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
          if (compareOrder(other, node) < 0) lo = mid + 1;
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
    if (
      this.pool &&
      [...this.pool.views].some(
        (view) =>
          view !== this &&
          (view.rootParent === id ||
            [...view.branches.values()].some((branch) =>
              branch.ids.includes(id),
            )),
      )
    )
      return;
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
    if (!branch?.open || branch.controller || (this.reading && !this.started))
      return;
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
      const page = await readResource<Page>(
        `/api/v1/sessions/${segment(this.sessionId)}/${this.endpoint(parent)}?${params}`,
        controller.signal,
      );
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
    const views = this.pool ? [...this.pool.views] : [this];
    if (
      !this.branches.has(parent) &&
      views.reduce((sum, view) => sum + view.branches.size, 0) >= MAX_BRANCHES
    ) {
      const dormant = views.find(
        (view) => view !== this && !view.started && view.branches.size,
      );
      if (dormant) {
        dormant.branches.clear();
        dormant.prune();
      }
    }
    if (
      !this.branches.has(parent) &&
      views.reduce((sum, view) => sum + view.branches.size, 0) >= MAX_BRANCHES
    ) {
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
    for (const child of branch.ids)
      if (this.branches.get(child)?.open) this.open(child);
  }
  close(parent: string) {
    const branch = this.branches.get(parent);
    if (!branch) return;
    branch.open = false;
    branch.controller?.abort();
    branch.controller = undefined;
    clearTimeout(branch.timer);
    const suspend = (id: string) => {
      const child = this.branches.get(id);
      if (!child) return;
      child.controller?.abort();
      child.controller = undefined;
      clearTimeout(child.timer);
      for (const nested of child.ids) suspend(nested);
    };
    for (const child of branch.ids) suspend(child);
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
  latest(parent: string) {
    this.releaseBranch(parent);
    this.open(parent);
  }
  start() {
    this.started = true;
    this.open(this.rootParent);
  }
  focus(parent: string) {
    this.stop();
    this.branches.clear();
    this.prune();
    this.rootParent = parent;
    this.started = true;
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
      this.branches.clear();
      this.prune();
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
    this.started = false;
    this.seekController?.abort();
    for (const branch of this.branches.values()) {
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
