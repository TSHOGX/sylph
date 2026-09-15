import { describe, expect, it, vi } from "vitest";
import {
  ExecutionStore,
  reportedUsage,
  type ExecutionNode,
} from "./execution-store";

function node(id: number, revision = id + 1): ExecutionNode {
  return {
    id: `n:${id}`,
    parentId: "",
    turnId: "turn",
    ordinal: id,
    orderTie: id + 1,
    bodyRevision: revision,
    revision,
    children: 0,
    tools: 0,
    reads: 0,
    commands: 0,
    thinking: 0,
    failed: 0,
    running: 0,
    waiting: 0,
    activity: "",
    event: {
      seq: id,
      kind: "assistant",
      text: `reply ${id}`,
      stability: "stable",
      source: "agent",
      createdAtMs: 1,
      arrivalAtMs: 1,
    },
  };
}
function seeded(count: number) {
  const store = new ExecutionStore("session");
  store.branches.set("", { ids: [], open: true, initialized: false });
  store.merge("", {
    nodes: Array.from({ length: count }, (_, id) => node(id)),
    snapshot: count,
    hasMore: false,
    nextCursor: null,
  });
  return store;
}
describe("execution entities", () => {
  it("restarting after cancellation starts a fresh initial request", async () => {
    let requests = 0;
    vi.stubGlobal("fetch", (_url: string, init: RequestInit) => {
      requests++;
      return new Promise((_resolve, reject) => {
        init.signal?.addEventListener("abort", () => reject(new Error("aborted")));
      });
    });
    const store = new ExecutionStore("session");
    try {
      store.start();
      store.stop();
      store.start();
      expect(requests).toBe(2);
    } finally {
      store.stop();
      await Promise.resolve();
      vi.unstubAllGlobals();
    }
  });
  it("a failed initial read retries the latest page before subscribing to revisions", async () => {
    vi.useFakeTimers();
    const urls: string[] = [];
    vi.stubGlobal("fetch", async (url: string) => {
      urls.push(url);
      return urls.length === 1
        ? new Response(null, { status: 503 })
        : Response.json({ nodes: [node(100)], snapshot: 101, hasMore: false, nextCursor: null });
    });
    const store = new ExecutionStore("session");
    try {
      store.start();
      await vi.advanceTimersByTimeAsync(2000);
      expect(urls).toHaveLength(2);
      expect(new URL(urls[1], "http://localhost").searchParams.has("changes")).toBe(false);
      expect(store.snapshot().map((row) => row.id)).toEqual(["n:100"]);
    } finally {
      store.stop();
      vi.unstubAllGlobals();
      vi.useRealTimers();
    }
  });
  it("evicting a collapsed child window preserves its visible summary", () => {
    const store = seeded(16);
    for (let id = 0; id < 15; id++) {
      const parent = `n:${id}`;
      store.branches.set(parent, { ids: [], open: false, initialized: true });
      store.merge(parent, {
        nodes: [{ ...node(100 + id), parentId: parent }],
        snapshot: 200, hasMore: false, nextCursor: null,
      });
    }
    const summary = store.nodes.get("n:0");
    // Open the uncached sixteenth summary with a fetch that stays pending.
    const original = globalThis.fetch;
    globalThis.fetch = () => new Promise(() => {});
    try {
      store.open("n:15");
      expect(store.nodes.get("n:0")).toBe(summary);
      expect(store.nodes.has("n:100")).toBe(false);
      expect(store.branches.has("n:0")).toBe(false);
      expect(store.snapshot().every(({ id }) => store.nodes.has(id))).toBe(true);
    } finally {
      store.stop();
      globalThis.fetch = original;
    }
  });
  it("removal patches release unreferenced nodes and their descendant windows", () => {
    const store = seeded(1);
    store.branches.set("n:0", { ids: [], open: true, initialized: false });
    store.merge("n:0", {
      nodes: [{ ...node(1), parentId: "n:0" }],
      snapshot: 2, hasMore: false, nextCursor: null,
    });
    store.merge("", {
      nodes: [{ ...node(0, 3), removed: true }],
      snapshot: 3, hasMore: false, nextCursor: null,
    }, true);
    expect(store.snapshot()).toEqual([]);
    expect(store.nodes.size).toBe(0);
    expect([...store.branches.keys()]).toEqual([""]);
  });
  it("one streaming patch preserves the visible list and every unrelated entity", () => {
    const store = seeded(100),
      list = store.snapshot(),
      unchanged = store.nodes.get("n:50");
    let changed = 0,
      unrelated = 0,
      structure = 0;
    store.subscribe("n:99", () => changed++);
    store.subscribe("n:50", () => unrelated++);
    store.subscribe("", () => structure++);
    store.merge(
      "",
      {
        nodes: [node(99, 101)],
        snapshot: 101,
        hasMore: false,
        nextCursor: null,
      },
      true,
    );
    expect(store.snapshot()).toBe(list);
    expect(store.nodes.get("n:50")).toBe(unchanged);
    expect([changed, unrelated, structure]).toEqual([1, 0, 0]);
  });
  it("a corrected ordering key repositions only the changed row", () => {
    const store = seeded(3);
    const unchanged = store.nodes.get("n:1");
    store.merge("", {
      nodes: [{ ...node(2, 10), ordinal: 0, orderTie: 0 }],
      snapshot: 10, hasMore: false, nextCursor: null,
    }, true);
    expect(store.snapshot().map((row) => row.id)).toEqual(["n:2", "n:0", "n:1"]);
    expect(store.nodes.get("n:1")).toBe(unchanged);
    store.merge("", {
      nodes: [node(2, 3)], snapshot: 3, hasMore: false, nextCursor: null,
    });
    expect(store.snapshot().map((row) => row.id)).toEqual(["n:2", "n:0", "n:1"]);
  });
  it("an older page cannot replace a correction received after its snapshot", () => {
    const store = seeded(2);
    const corrected = node(1, 20);
    store.merge(
      "",
      { nodes: [corrected], snapshot: 20, hasMore: false, nextCursor: null },
      true,
    );
    store.merge("", {
      nodes: [node(1, 2)],
      snapshot: 2,
      hasMore: false,
      nextCursor: null,
    });
    expect(store.nodes.get("n:1")).toBe(corrected);
    expect(store.snapshot().map((n) => n.id)).toEqual(["n:0", "n:1"]);
  });
  it("unloaded historical changes do not grow the entity cache", () => {
    const store = seeded(1);
    store.branches.get("")!.cursor = "0:n:0";
    for (let i = 1; i <= 1000; i++)
      store.merge(
        "",
        {
          nodes: [node(-i, i + 2)],
          snapshot: i + 2,
          hasMore: false,
          nextCursor: null,
        },
        true,
      );
    expect(store.nodes.size).toBe(1);
  });
  it("live cache eviction keeps discarded history reachable by pagination", () => {
    const store = seeded(160);
    store.merge("", {
      nodes: [node(160)], snapshot: 161, hasMore: false, nextCursor: null,
    }, true);
    expect(store.nodes.size).toBe(160);
    expect(store.branches.get("")!.cursor).toBe("1:2:n:1");
    expect(store.snapshot()[0].id).toBe("n:1");
    store.merge("", {
      nodes: [node(0)], snapshot: 161, hasMore: false, nextCursor: null,
    });
    expect(store.snapshot()[0].id).toBe("n:0");
  });
  it("pagination advances a bounded window instead of discarding the older page", () => {
    const store = seeded(160);
    store.merge("", {
      nodes: [node(-1)],
      snapshot: 161,
      hasMore: true,
      nextCursor: "-1:n:-1",
    });
    expect(store.nodes.size).toBe(160);
    expect(store.snapshot()[0].id).toBe("n:-1");
    expect(store.nodes.has("n:159")).toBe(false);
  });
  it("live arrivals do not splice a disconnected tail into a historical window", () => {
    const store = seeded(160);
    store.merge("", {
      nodes: [node(-1)], snapshot: 160, hasMore: true, nextCursor: "-1:0:n:-1",
    });
    const before = store.snapshot();
    store.merge("", {
      nodes: [node(200, 201), node(50, 202)],
      snapshot: 202, hasMore: false, nextCursor: null,
    }, true);
    expect(store.snapshot()).toBe(before);
    expect(store.nodes.has("n:200")).toBe(false);
    expect(store.nodes.get("n:50")!.revision).toBe(202);
  });
  it("the unloaded history boundary compares every ordering component", () => {
    const store = seeded(1);
    store.branches.get("")!.cursor = "0:1:n:0";
    store.merge("", {
      nodes: [{ ...node(1, 3), ordinal: 0, orderTie: 0 }],
      snapshot: 3, hasMore: false, nextCursor: null,
    }, true);
    expect(store.nodes.size).toBe(1);
  });
});

it("a move removes the old row even when the destination patch arrived first", () => {
  const store = seeded(1);
  store.branches.set("other", { ids: [], open: true, initialized: true });
  const moved = { ...node(0, 10), parentId: "other" };
  store.merge(
    "other",
    { nodes: [moved], snapshot: 10, hasMore: false, nextCursor: null },
    true,
  );
  store.merge(
    "",
    {
      nodes: [{ ...node(0, 10), removed: true }],
      snapshot: 10,
      hasMore: false,
      nextCursor: null,
    },
    true,
  );
  expect(store.branches.get("")!.ids).toEqual([]);
  expect(store.branches.get("other")!.ids).toEqual(["n:0"]);
  expect(store.nodes.get("n:0")).toBe(moved);
});

it("reported usage preserves vendor keys, scope, real zeros and missing values", () => {
  expect(reportedUsage({ input_tokens: 10, output_tokens: 20 })).toBe(
    "10 input tokens · 20 output tokens",
  );
  expect(
    reportedUsage({
      total: { inputTokens: 12, outputTokens: 7 },
      last: { inputTokens: 3, outputTokens: 2 },
    }),
  ).toBe("Reported total: 12 input tokens · 7 output tokens");
  expect(
    reportedUsage({
      total_token_usage: { input_tokens: 13, output_tokens: 8 },
    }),
  ).toBe("Reported total: 13 input tokens · 8 output tokens");
  expect(reportedUsage({ outputTokens: 0 })).toBe("0 output tokens");
  expect(reportedUsage({})).toBe("Usage was not reported");
});
