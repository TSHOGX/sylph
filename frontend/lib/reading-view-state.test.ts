import { readingView } from "./reading-view-state";
import { expect, test, vi } from "vitest";
import { ExecutionStore, type ExecutionNode } from "./execution-store";
const node: ExecutionNode = {
  id: "reply",
  parentId: "",
  turnId: "turn",
  ordinal: 1,
  orderTie: 1,
  revision: 1,
  bodyRevision: 1,
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
    seq: 1,
    kind: "assistant",
    text: "Shared answer",
    source: "agent",
    stability: "stable",
    createdAtMs: 1,
  },
};
test("readers share entities while keeping navigation and lifetime independent", () => {
  const main = new ExecutionStore("shared-reading", true);
  const side = new ExecutionStore("shared-reading", true);
  for (const store of [main, side]) {
    store.branches.set("", { ids: [], open: true, initialized: false });
    store.merge("", {
      nodes: [node],
      snapshot: 1,
      nextCursor: null,
      hasMore: false,
    });
  }
  expect(main.nodes).toBe(side.nodes);
  side.rootParent = "child";
  expect(main.rootParent).toBe("");
  side.dispose();
  expect(main.nodes.get("reply")?.event.text).toBe("Shared answer");
  main.dispose();
});

test("expanding work preserves steering between the surrounding process items", () => {
  const store = new ExecutionStore("ordered-reading", true);
  store.branches.set("", { ids: [], open: true, initialized: false });
  const item = (id: string, ordinal: number, kind: string): ExecutionNode => ({
    ...node,
    id,
    ordinal,
    orderTie: ordinal,
    event: { ...node.event, kind },
  });
  store.merge("", {
    nodes: [
      item("work", 1, "work"),
      item("steering", 3, "user"),
      item("answer", 5, "assistant"),
    ],
    snapshot: 1,
    nextCursor: null,
    hasMore: false,
  });
  store.branches.set("work", { ids: [], open: true, initialized: false });
  store.merge("work", {
    nodes: [item("progress", 2, "assistant"), item("group", 4, "execution")],
    snapshot: 1,
    nextCursor: null,
    hasMore: false,
  });
  expect(store.snapshot().map((row) => row.id)).toEqual([
    "work",
    "progress",
    "steering",
    "group",
    "answer",
  ]);
  store.dispose();
});

test("visiting many delegates never evicts the visible main reader", () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(() => new Promise(() => {})),
  );
  const main = readingView("visible-main", "", "main");
  try {
    main.start();
    main.merge("", {
      nodes: [node],
      snapshot: 1,
      nextCursor: null,
      hasMore: false,
    });
    for (let i = 0; i < 20; i++)
      readingView("visible-main", `delegate-${i}`, "panel");
    expect(
      readingView("visible-main", "", "main")
        .snapshot()
        .map((row) => row.id),
    ).toEqual(["reply"]);
    for (let i = 0; i < 20; i++)
      readingView(`related-session-${i}`, "", "panel");
    expect(
      readingView("visible-main", "", "main")
        .snapshot()
        .map((row) => row.id),
    ).toEqual(["reply"]);
  } finally {
    main.dispose();
    vi.unstubAllGlobals();
  }
});

test("interacting with a tool group keeps its reading parent open on completion", () => {
  const store = new ExecutionStore("reading-intent", true);
  const work = { ...node, id: "work", event: { ...node.event, kind: "work" } };
  const group = {
    ...node,
    id: "group",
    event: { ...node.event, kind: "execution" },
  };
  store.branches.set("", { ids: [], open: true, initialized: false });
  store.merge("", {
    nodes: [work],
    snapshot: 1,
    nextCursor: null,
    hasMore: false,
  });
  store.branches.set("work", { ids: [], open: true, initialized: false });
  store.merge("work", {
    nodes: [group],
    snapshot: 1,
    nextCursor: null,
    hasMore: false,
  });
  store.chooseDisclosure("group", true);
  expect(store.disclosureChoices.get("work")).toBe(true);
  store.chooseDisclosure("work", false);
  expect(store.disclosureChoices.get("work")).toBe(false);
  store.dispose();
});
