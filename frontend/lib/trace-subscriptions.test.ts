import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { clearTraceAccess } from "./trace-cache";
import { subscribeTrace } from "./trace-subscriptions";
import type { TraceChange, TraceLimits, TraceView } from "./trace-types";

const limits = {
  maxVisibleInterests: 4,
  maxSummaryBytes: 65536,
  maxChangeItems: 32,
} as TraceLimits;
const view = (
  token: string,
  generation: string,
  viewRevision: number,
): TraceView => ({
  token,
  generation,
  viewRevision,
  evidenceCut: viewRevision,
  expiresAtMs: 9999999999999,
});
const revision = (
  generation: string,
  value: number,
  agents: string[],
): TraceChange => ({
  kind: "revision",
  generation,
  viewRevision: value,
  evidenceCut: value,
  cursor: `${generation}:${value}`,
  agents,
  changed: [],
  moved: [],
  removed: [],
});
interface Connection {
  url: URL;
  emit: (change: TraceChange) => void;
  aborted: () => boolean;
}
let connections: Connection[];

beforeEach(() => {
  clearTraceAccess();
  connections = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: string, options: RequestInit) => {
      let closed = false;
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          connections.push({
            url: new URL(input, "http://fixture"),
            emit: (change) => {
              if (!closed)
                controller.enqueue(
                  new TextEncoder().encode(
                    `data: ${JSON.stringify(change)}\n\n`,
                  ),
                );
            },
            aborted: () => Boolean(options.signal?.aborted),
          });
          options.signal?.addEventListener(
            "abort",
            () => {
              if (!closed) {
                closed = true;
                controller.close();
              }
            },
            { once: true },
          );
        },
        cancel() {
          closed = true;
        },
      });
      return Promise.resolve(
        new Response(stream, {
          headers: { "Content-Type": "text/event-stream" },
        }),
      );
    }),
  );
});
afterEach(() => {
  clearTraceAccess();
  vi.unstubAllGlobals();
});
const connect = (agent: string, snapshot: TraceView, listener = vi.fn()) => ({
  stop: subscribeTrace(
    agent,
    snapshot,
    `${snapshot.generation}:${snapshot.viewRevision}`,
    limits,
    listener,
  ),
  listener,
});
const last = () => connections.at(-1)!;
const proofs = () =>
  JSON.parse(last().url.searchParams.get("views")!) as Array<{
    agent: string;
    atSnapshot: string;
  }>;

it("a new generation resumes from a fresh bootstrap while the other reader keeps its reset notice", async () => {
  const first = connect("first", view("first-old", "old", 4));
  const second = connect("second", view("second-old", "old", 6));
  last().emit({ kind: "reset_required", reason: "Generation replaced" });
  await vi.waitFor(() =>
    expect(first.listener).toHaveBeenCalledWith({
      kind: "reset_required",
      reason: "Generation replaced",
    }),
  );
  expect(second.listener).toHaveBeenCalledWith({
    kind: "reset_required",
    reason: "Generation replaced",
  });
  first.stop();
  const fresh = connect("first", view("first-new", "new", 9));
  expect(last().url.searchParams.get("cursor")).toBe("new:9");
  expect(proofs()).toEqual(
    expect.arrayContaining([
      { agent: "first", atSnapshot: "first-new" },
      { agent: "second", atSnapshot: "second-old" },
    ]),
  );
  last().emit(revision("new", 10, ["first"]));
  await vi.waitFor(() =>
    expect(fresh.listener).toHaveBeenCalledWith(revision("new", 10, ["first"])),
  );
  second.stop();
  const renewed = connect("second", view("second-new", "new", 8));
  expect(last().url.searchParams.get("cursor")).toBe("new:8");
  last().emit(revision("new", 9, ["second"]));
  await vi.waitFor(() =>
    expect(renewed.listener).toHaveBeenCalledWith(
      revision("new", 9, ["second"]),
    ),
  );
});

it("a missing replay prefix cannot reconnect from a rejected view before a new bootstrap arrives", async () => {
  const first = connect("first", view("first-stale", "generation", 2));
  connect("second", view("second-stale", "generation", 3));
  last().emit({ kind: "reset_required", reason: "Replay prefix removed" });
  await vi.waitFor(() => expect(first.listener).toHaveBeenCalled());
  const count = connections.length;
  first.stop();
  const stale = connect("first", view("first-stale", "generation", 2));
  expect(connections).toHaveLength(count);
  stale.stop();
  connect("first", view("first-fresh", "generation", 20));
  expect(last().url.searchParams.get("cursor")).toBe("generation:20");
  connect("second", view("second-stale", "generation", 3));
  expect(last().url.searchParams.get("cursor")).toBe("generation:20");
});

it("adding a newer reader cannot skip invalidations still owed to an older reader", async () => {
  const first = connect("first", view("first", "generation", 10));
  connect("second", view("second", "generation", 40));
  expect(last().url.searchParams.get("cursor")).toBe("generation:10");
  expect(proofs()).toHaveLength(2);
  last().emit(revision("generation", 11, ["first"]));
  await vi.waitFor(() =>
    expect(first.listener).toHaveBeenCalledWith(
      revision("generation", 11, ["first"]),
    ),
  );
});

it("two snapshots of one agent retain independent scope proofs through a reset handoff", async () => {
  const paused = connect("agent", view("paused", "old", 1));
  const current = connect("agent", view("current", "old", 3));
  expect(proofs()).toHaveLength(2);
  last().emit({ kind: "reset_required", reason: "Generation replaced" });
  await vi.waitFor(() => expect(current.listener).toHaveBeenCalled());
  current.stop();
  connect("agent", view("fresh", "new", 7));
  expect(last().url.searchParams.get("cursor")).toBe("new:7");
  expect(proofs()).toEqual([
    { agent: "agent", atSnapshot: "paused" },
    { agent: "agent", atSnapshot: "fresh" },
  ]);
  paused.stop();
  expect(proofs()).toEqual([{ agent: "agent", atSnapshot: "fresh" }]);
});

it("a listener's synchronous bootstrap handoff is not stopped by the old reset handler", async () => {
  const recovered = vi.fn();
  const first = connect(
    "first",
    view("first-old", "old", 2),
    vi.fn((change: TraceChange) => {
      if (change.kind === "reset_required") {
        first.stop();
        connect("first", view("first-new", "new", 8), recovered);
      }
    }),
  );
  connect("second", view("second-old", "old", 4));
  last().emit({ kind: "reset_required", reason: "Generation replaced" });
  await vi.waitFor(() =>
    expect(last().url.searchParams.get("cursor")).toBe("new:8"),
  );
  expect(last().aborted()).toBe(false);
  last().emit(revision("new", 9, ["first"]));
  await vi.waitFor(() =>
    expect(recovered).toHaveBeenCalledWith(revision("new", 9, ["first"])),
  );
});

it("a bootstrap from a replacement generation invalidates the old readers before replaying its cursor", () => {
  const previous = connect("first", view("old", "generation-one", 5));
  connect("second", view("new", "generation-two", 8));
  expect(previous.listener).toHaveBeenCalledWith(
    expect.objectContaining({ kind: "reset_required" }),
  );
  expect(last().url.searchParams.get("cursor")).toBe("generation-two:8");
  expect(proofs()).toHaveLength(2);
});
