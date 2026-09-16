import { describe, expect, it } from "vitest";
import { createSSEDecoder } from "./sse-parse";

describe("bounded trace change framing", () => {
  it("rejects an unterminated response before its buffer grows past the bound", () => {
    const decoder = createSSEDecoder(20);
    decoder.push("data: partial");
    expect(() => decoder.push("more bytes beyond the bound")).toThrow(
      "SSE buffer exceeded",
    );
  });
  it("counts data across individually short lines in one frame", () => {
    const decoder = createSSEDecoder(20);
    decoder.push("data: abcdef\n");
    expect(() => decoder.push("data: ghijkl\n")).toThrow("SSE frame exceeded");
  });
  it("releases the completed frame budget before receiving the next revision", () => {
    const decoder = createSSEDecoder(20);
    expect(decoder.push("data: abcdef\n\n")[0].data).toBe("abcdef");
    expect(decoder.push("data: ghijkl\n\n")[0].data).toBe("ghijkl");
  });
});
