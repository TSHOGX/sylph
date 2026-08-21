import { describe, expect, it } from "vitest";
import { createSSEDecoder } from "@/lib/sse-parse";

/**
 * Event-stream frame decoding.
 *
 * The bug this file exists to prevent: the backend names every frame after its
 * event kind (`event: thinking`), and the previous client listened only on
 * `EventSource.onmessage`, which by spec receives only frames named `message`.
 * Every mid-turn event was therefore dropped and the transcript appeared to
 * arrive in one lump when the turn finished. The first test below is that bug
 * stated as a promise: a named frame must reach the consumer.
 *
 * Chunk-splitting cases matter as much. A real stream splits wherever TCP
 * decides, so a decoder that only works on whole frames works right up until it
 * silently eats a token in production.
 */
describe("createSSEDecoder", () => {
  it("delivers frames whose event name is not 'message'", () => {
    // The regression. Every frame the backend emits is named.
    const decoder = createSSEDecoder();
    const frames = decoder.push(
      'event: thinking\nid: 3\ndata: {"seq":3,"kind":"thinking"}\n\n',
    );

    expect(frames).toHaveLength(1);
    expect(frames[0].event).toBe("thinking");
    expect(frames[0].id).toBe("3");
    expect(JSON.parse(frames[0].data)).toMatchObject({ seq: 3, kind: "thinking" });
  });

  it("defaults the event name to 'message' when the frame does not name one", () => {
    const frames = createSSEDecoder().push("data: bare\n\n");
    expect(frames).toHaveLength(1);
    expect(frames[0].event).toBe("message");
  });

  it("reassembles a frame split across chunks at an arbitrary byte", () => {
    // TCP splits wherever it likes, including mid-field and mid-JSON.
    const decoder = createSSEDecoder();
    expect(decoder.push("event: assist")).toEqual([]);
    expect(decoder.push('ant\ndata: {"se')).toEqual([]);
    expect(decoder.push('q":7}')).toEqual([]);
    const frames = decoder.push("\n\n");

    expect(frames).toHaveLength(1);
    expect(frames[0].event).toBe("assistant");
    expect(JSON.parse(frames[0].data)).toMatchObject({ seq: 7 });
  });

  it("returns several frames when one chunk completes more than one", () => {
    // A fast agent's deltas coalesce into a single read.
    const frames = createSSEDecoder().push(
      "event: a\ndata: 1\n\nevent: b\ndata: 2\n\nevent: c\ndata: 3\n\n",
    );
    expect(frames.map((f) => f.event)).toEqual(["a", "b", "c"]);
    expect(frames.map((f) => f.data)).toEqual(["1", "2", "3"]);
  });

  it("does not dispatch a frame until its terminating blank line arrives", () => {
    // Dispatching early would render a half-parsed event.
    const decoder = createSSEDecoder();
    expect(decoder.push("event: assistant\ndata: partial\n")).toEqual([]);
    expect(decoder.push("\n")).toHaveLength(1);
  });

  it("joins multiple data lines with newlines", () => {
    const frames = createSSEDecoder().push("data: line one\ndata: line two\n\n");
    expect(frames[0].data).toBe("line one\nline two");
  });

  it("ignores comment lines, so keep-alives do not surface as events", () => {
    // The backend sends `:` keep-alives every 15s to hold the connection open.
    const decoder = createSSEDecoder();
    expect(decoder.push(":\n\n")).toEqual([]);
    expect(decoder.push(": keep-alive\n\n")).toEqual([]);
    expect(decoder.push("event: assistant\ndata: real\n\n")).toHaveLength(1);
  });

  it("strips exactly one leading space after the colon", () => {
    // `data:  x` carries a value of " x": only the first space is a separator.
    const frames = createSSEDecoder().push("data:  padded\n\n");
    expect(frames[0].data).toBe(" padded");
  });

  it("accepts a field with no colon as an empty value", () => {
    const frames = createSSEDecoder().push("data\ndata: after\n\n");
    expect(frames[0].data).toBe("\nafter");
  });

  it("terminates lines on CRLF and on a lone CR", () => {
    expect(createSSEDecoder().push("event: a\r\ndata: crlf\r\n\r\n")[0].data).toBe("crlf");

    // A lone CR terminates a line only once something after it proves it was not
    // the first half of a CRLF pair. Here the following `e` settles it, so both
    // the data line and the dispatching blank line resolve.
    const frames = createSSEDecoder().push("event: b\rdata: cr\r\rend");
    expect(frames).toHaveLength(1);
    expect(frames[0].data).toBe("cr");
    expect(frames[0].event).toBe("b");
  });

  it("waits for the newline of a CRLF pair split across chunks", () => {
    // Treating the trailing CR as a line end would split one terminator into
    // two blank lines and dispatch an empty frame.
    const decoder = createSSEDecoder();
    expect(decoder.push("data: x\r")).toEqual([]);
    const frames = decoder.push("\n\r\n");
    expect(frames).toHaveLength(1);
    expect(frames[0].data).toBe("x");
  });

  it("carries the last id forward to frames that omit one", () => {
    // Per spec the id is stream state, not frame state. It backs reconnect
    // resume, so losing it would replay from the start of the turn.
    const decoder = createSSEDecoder();
    const first = decoder.push("id: 5\ndata: a\n\n");
    const second = decoder.push("data: b\n\n");
    expect(first[0].id).toBe("5");
    expect(second[0].id).toBe("5");
  });

  it("reads a numeric retry and ignores a malformed one", () => {
    expect(createSSEDecoder().push("retry: 2500\ndata: x\n\n")[0].retry).toBe(2500);
    expect(createSSEDecoder().push("retry: soon\ndata: x\n\n")[0].retry).toBeUndefined();
  });

  it("ignores unknown fields rather than failing the frame", () => {
    const frames = createSSEDecoder().push("event: a\nfuture: 1\ndata: x\n\n");
    expect(frames).toHaveLength(1);
    expect(frames[0].data).toBe("x");
  });

  it("does not let one frame's event name leak into the next", () => {
    const decoder = createSSEDecoder();
    const frames = decoder.push("event: thinking\ndata: 1\n\ndata: 2\n\n");
    expect(frames[0].event).toBe("thinking");
    expect(frames[1].event).toBe("message");
  });
});
