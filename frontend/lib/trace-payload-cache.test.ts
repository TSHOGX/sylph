import { expect, it } from "vitest";
import { PayloadCache } from "./trace-payload-cache";

it("an open body cannot be evicted to conceal a payload budget overrun", () => {
  const cache = new PayloadCache();
  cache.configure(10);
  expect(cache.acquire("a", 8)).toBe(true);
  cache.set("a", new Uint8Array(8));
  expect(cache.acquire("b", 8)).toBe(false);
  expect(cache.get("a")).toHaveLength(8);
  expect(cache.bytes).toBeLessThanOrEqual(10);
  cache.release("a");
  expect(cache.acquire("b", 8)).toBe(true);
  expect(cache.get("a")).toBeUndefined();
});
it("two readers share one immutable body until the last reader closes", () => {
  const cache = new PayloadCache();
  cache.configure(10);
  cache.acquire("a", 8);
  cache.set("a", new Uint8Array(8));
  cache.acquire("a", 8);
  cache.release("a");
  expect(cache.acquire("b", 8)).toBe(false);
  cache.release("a");
  expect(cache.acquire("b", 8)).toBe(true);
});
it("a response cannot replace immutable bytes or exceed its reserved range", () => {
  const cache = new PayloadCache();
  cache.configure(10);
  cache.acquire("a", 4);
  cache.set("a", new Uint8Array([1, 2, 3, 4]));
  expect(() => cache.set("a", new Uint8Array([5, 6, 7, 8]))).toThrow(
    "without an immutable revision",
  );
  expect(() => cache.set("a", new Uint8Array(8))).toThrow(
    "reserved byte range",
  );
});
