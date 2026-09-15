import { afterEach, expect, test, vi } from "vitest";
import { readResource } from "./reading-resource";
afterEach(() => vi.unstubAllGlobals());

test("two readers share a request and one can leave without cancelling the other", async () => {
  let finish!: (value: unknown) => void;
  let signal!: AbortSignal;
  const fetch = vi.fn((_url: string, options: { signal: AbortSignal }) => {
    signal = options.signal;
    return new Promise((resolve) => {
      finish = resolve;
    });
  });
  vi.stubGlobal("fetch", fetch);
  const first = new AbortController();
  const second = new AbortController();
  const a = readResource("/shared", first.signal);
  const b = readResource("/shared", second.signal);
  const failure = expect(a).rejects.toMatchObject({ name: "AbortError" });
  first.abort();
  await failure;
  expect(signal.aborted).toBe(false);
  expect(fetch).toHaveBeenCalledTimes(1);
  finish({ ok: true, json: async () => ({ value: "complete" }) });
  await expect(b).resolves.toEqual({ value: "complete" });
});

test("the last reader leaving cancels the transport", async () => {
  let signal!: AbortSignal;
  vi.stubGlobal(
    "fetch",
    vi.fn((_url: string, options: { signal: AbortSignal }) => {
      signal = options.signal;
      return new Promise(() => {});
    }),
  );
  const controller = new AbortController();
  const read = readResource("/abandoned", controller.signal);
  const failure = expect(read).rejects.toMatchObject({ name: "AbortError" });
  controller.abort();
  await failure;
  expect(signal.aborted).toBe(true);
});
