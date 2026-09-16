import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  clearTraceLeases,
  discardUnusedTraceView,
  retainTraceView,
} from "./trace-leases";
import type { TraceView } from "./trace-types";
const view: TraceView = {
  token: "shared-view",
  generation: "generation",
  evidenceCut: 1,
  viewRevision: 1,
  expiresAtMs: 1000,
};
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(new Response(null, { status: 204 })),
  );
});
afterEach(() => {
  clearTraceLeases();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
it("a view remains pinned until the last visible consumer releases it", async () => {
  const first = retainTraceView(view),
    second = retainTraceView(view);
  first();
  await vi.runAllTimersAsync();
  expect(fetch).not.toHaveBeenCalled();
  second();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledOnce();
  expect(fetch).toHaveBeenCalledWith(
    "/api/v1/trace/views/shared-view",
    expect.objectContaining({ method: "DELETE" }),
  );
});
it("a synchronous navigation handoff cancels the obsolete release task", async () => {
  const previous = retainTraceView(view);
  previous();
  const next = retainTraceView(view);
  await vi.runAllTimersAsync();
  expect(fetch).not.toHaveBeenCalled();
  next();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledOnce();
});
it("an abandoned response releases its new lease without releasing a shared view", async () => {
  const held = retainTraceView(view);
  discardUnusedTraceView(view);
  discardUnusedTraceView({ ...view, token: "abandoned-view" });
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch).toHaveBeenCalledWith(
    "/api/v1/trace/views/abandoned-view",
    expect.anything(),
  );
  held();
  held();
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(2);
});
