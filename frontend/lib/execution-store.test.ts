import { describe, expect, it } from "vitest";
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
