import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { TraceView } from "./trace-types";

const view: TraceView = {
  token: "visible-view",
  generation: "generation",
  evidenceCut: 1,
  viewRevision: 1,
  expiresAtMs: Date.now() + 60_000,
};
let page: EventTarget;

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  page = new EventTarget();
  vi.stubGlobal("window", page);
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(new Response(null, { status: 204 })),
  );
});
afterEach(async () => {
  const { clearTraceAccess } = await import("./trace-cache");
  clearTraceAccess();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it("clearing protected caches cancels active and pending leases before discarding ownership", async () => {
  const { retainTraceView } = await import("./trace-leases");
  const { clearTraceAccess } = await import("./trace-cache");
  const visible = retainTraceView(view);
  const pending = retainTraceView({ ...view, token: "pending-view" });
  pending();

  clearTraceAccess();

  expect(fetch).toHaveBeenCalledTimes(2);
  expect(fetch).toHaveBeenCalledWith(
    "/api/v1/trace/views/pending-view",
    expect.objectContaining({ method: "DELETE", keepalive: true }),
  );
  expect(fetch).toHaveBeenCalledWith(
    "/api/v1/trace/views/visible-view",
    expect.objectContaining({ method: "DELETE", keepalive: true }),
  );
  visible();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("document departure releases mounted views without waiting for React cleanup or another task", async () => {
  const { retainTraceView } = await import("./trace-leases");
  const visible = retainTraceView(view);
  const pending = retainTraceView({ ...view, token: "pending-view" });
  pending();

  page.dispatchEvent(new Event("pagehide"));

  expect(fetch).toHaveBeenCalledTimes(2);
  visible();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("a response delivered after pagehide cannot acquire a new abandoned lease", async () => {
  const { retainTraceView, discardUnusedTraceView } = await import("./trace-leases");
  page.dispatchEvent(new Event("pagehide"));

  const late = retainTraceView(view);
  discardUnusedTraceView({ ...view, token: "abandoned-response" });

  expect(fetch).toHaveBeenCalledTimes(2);
  late();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("restoring a retained document reacquires views while preserving its unsent draft", async () => {
  const { retainTraceView } = await import("./trace-leases");
  const { draftFor, onTraceAccessCleared, saveDraft } = await import("./trace-cache");
  const invalidated = vi.fn();
  const stop = onTraceAccessCleared(invalidated);
  saveDraft("agent", "Still unsent");
  retainTraceView(view);
  page.dispatchEvent(new Event("pagehide"));
  expect(fetch).toHaveBeenCalledOnce();
  page.dispatchEvent(Object.assign(new Event("pageshow"), { persisted: true }));
  expect(invalidated).toHaveBeenCalledExactlyOnceWith("view");
  expect(draftFor("agent")).toBe("Still unsent");

  const restored = retainTraceView({ ...view, token: "restored-view" });
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledOnce();
  restored();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(2);
  stop();
});

it("an ordinary pageshow keeps the current authorized view", async () => {
  const { retainTraceView } = await import("./trace-leases");
  const { onTraceAccessCleared } = await import("./trace-cache");
  const invalidated = vi.fn();
  const stop = onTraceAccessCleared(invalidated);
  retainTraceView(view);
  page.dispatchEvent(Object.assign(new Event("pageshow"), { persisted: false }));
  await vi.runAllTimersAsync();
  expect(fetch).not.toHaveBeenCalled();
  expect(invalidated).not.toHaveBeenCalled();
  stop();
});
