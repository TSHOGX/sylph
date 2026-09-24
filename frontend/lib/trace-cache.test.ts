import { afterEach, describe, expect, it } from "vitest";
import {
  authorizeTraceCache,
  ByteCache,
  clearTraceAccess,
  onTraceAccessCleared,
  traceMetadata,
  tracePayloadKey,
} from "./trace-cache";
import type { PayloadAccess, TraceLimits } from "./trace-types";

const access: PayloadAccess = {
  authority: { kind: "projection" },
  payloadRef: "retained-payload",
  bodyRevision: "revision-one",
  representation: "caller_result",
  viaRef: "readable-operation",
  containingAgent: "agent-one",
  selection: null,
  mediaType: "text/plain",
  encoding: "utf8",
  totalBytes: 100,
  availableRanges: [{ start: 0, endExclusive: 100 }],
  availability: { kind: "available" },
};
const limits = {
  maxMetadataCacheBytes: 64,
  maxPayloadCacheBytes: 128,
} as TraceLimits;
afterEach(clearTraceAccess);

describe("bounded trace caches", () => {
  it("accounts for keys and evicts the least recently read value", () => {
    const cache = new ByteCache<string>(10);
    cache.set("a", "first", 4);
    cache.set("b", "second", 4);
    expect(cache.bytes).toBe(10);
    expect(cache.get("a")).toBe("first");
    cache.set("c", "third", 4);
    expect(cache.get("b")).toBeUndefined();
    expect(cache.get("a")).toBe("first");
    expect(cache.bytes).toBe(10);
  });
  it("cannot populate a disabled budget through zero-length entries", () => {
    const cache = new ByteCache<string>(0);
    cache.set("", "empty", 0);
    expect(cache.get("")).toBeUndefined();
    expect(cache.bytes).toBe(0);
  });
  it("drops an oversized replacement and rejects invalid byte accounting", () => {
    const cache = new ByteCache<string>(8);
    cache.set("id", "old", 2);
    cache.set("id", "large", 12);
    expect(cache.get("id")).toBeUndefined();
    expect(cache.bytes).toBe(0);
    expect(() => cache.set("bad", "bad", -1)).toThrow(
      "Invalid cache byte accounting",
    );
  });
  it("changing authorization clears protected values and notifies mounted readers", () => {
    let cleared = 0;
    const stop = onTraceAccessCleared(() => cleared++);
    authorizeTraceCache("subject-one", limits);
    traceMetadata.set("visible", { secret: true }, 10);
    authorizeTraceCache("subject-two", limits);
    expect(traceMetadata.get("visible")).toBeUndefined();
    expect(cleared).toBe(1);
    stop();
  });
  it("immutable body keys retain the authorization anchor and representation", () => {
    authorizeTraceCache("subject-one", limits);
    const first = tracePayloadKey(access, 0);
    expect(tracePayloadKey({ ...access }, 0)).toBe(first);
    expect(
      tracePayloadKey({ ...access, representation: "runtime" }, 0),
    ).not.toBe(first);
    expect(
      tracePayloadKey({ ...access, viaRef: "other-recipient" }, 0),
    ).not.toBe(first);
    authorizeTraceCache("subject-two", limits);
    expect(tracePayloadKey(access, 0)).not.toBe(first);
  });
});
