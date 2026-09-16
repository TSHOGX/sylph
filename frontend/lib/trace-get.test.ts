import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "./api";
import { readTraceWhenReady } from "./trace-get";

afterEach(() => vi.useRealTimers());
describe("projection readiness reads", () => {
  it("retries a declared pending projection and returns the ready response", async () => {
    vi.useFakeTimers();
    const read = vi
      .fn()
      .mockRejectedValueOnce(
        new ApiError(503, "projection_pending", "Not ready"),
      )
      .mockResolvedValue({ revision: 2 });
    const result = readTraceWhenReady(read, new AbortController().signal);
    await vi.runAllTimersAsync();
    await expect(result).resolves.toEqual({ revision: 2 });
    expect(read).toHaveBeenCalledTimes(2);
  });
  it("does not repeat authorization failures or arbitrary server errors", async () => {
    const error = new ApiError(403, "forbidden", "Access removed");
    const read = vi.fn().mockRejectedValue(error);
    await expect(
      readTraceWhenReady(read, new AbortController().signal),
    ).rejects.toBe(error);
    expect(read).toHaveBeenCalledTimes(1);
  });
  it("aborting a pending index read cancels its retry timer", async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    const read = vi
      .fn()
      .mockRejectedValue(new ApiError(503, "search_indexing", "Index pending"));
    const result = readTraceWhenReady(read, controller.signal);
    const rejection = expect(result).rejects.toThrow("Aborted read");
    await vi.advanceTimersByTimeAsync(1);
    controller.abort(new Error("Aborted read"));
    await rejection;
    await vi.runAllTimersAsync();
    expect(read).toHaveBeenCalledTimes(1);
  });
});
