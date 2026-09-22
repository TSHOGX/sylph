import { describe, expect, it, vi } from "vitest";
import {
  receiveTransfer,
  reserveTransfer,
  type TransferSink,
} from "./trace-transfer";
function response(
  chunks: Uint8Array[],
  observed = chunks.reduce((sum, chunk) => sum + chunk.length, 0),
  maximum = 32 * 1024 * 1024,
) {
  return new Response(
    new ReadableStream({
      start(controller) {
        for (const chunk of chunks) controller.enqueue(chunk);
        controller.close();
      },
    }),
    {
      headers: {
        "x-sylph-observed-bytes": String(observed),
        "x-sylph-transfer-limit": String(maximum),
      },
    },
  );
}
function sink(): TransferSink {
  return {
    write: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    abort: vi.fn(async () => {}),
  };
}
describe("explicit observed-content transfer", () => {
  it("keeps a UTF-8 BOM and scalars split across transport chunks", async () => {
    const text = "\ufeffα中🙂\r\nretained";
    const bytes = new TextEncoder().encode(text);
    const chunks = Array.from(bytes, (byte) => Uint8Array.of(byte));
    expect(
      await receiveTransfer(
        response(chunks),
        "copy",
        new AbortController().signal,
        () => {},
      ),
    ).toBe(text);
  });
  it("copies a twenty-megabyte response outside the reader cache with incremental progress", async () => {
    const text = "retained data\n".repeat(Math.ceil((20 * 1024 * 1024) / 14));
    const bytes = new TextEncoder().encode(text);
    const progress = vi.fn();
    const parts = [];
    for (let offset = 0; offset < bytes.length; offset += 1024 * 1024)
      parts.push(bytes.subarray(offset, offset + 1024 * 1024));
    expect(
      await receiveTransfer(
        response(parts),
        "copy",
        new AbortController().signal,
        progress,
      ),
    ).toBe(text);
    expect(progress.mock.calls.length).toBe(parts.length);
    expect(progress.mock.calls.at(-1)).toEqual([bytes.length, bytes.length]);
  });
  it("refuses a short body and invalid UTF-8 instead of returning plausible text", async () => {
    await expect(
      receiveTransfer(
        response([Uint8Array.of(65)], 2),
        "copy",
        new AbortController().signal,
        () => {},
      ),
    ).rejects.toThrow("before all observed");
    await expect(
      receiveTransfer(
        response([Uint8Array.of(0xf0, 0x9f)]),
        "copy",
        new AbortController().signal,
        () => {},
      ),
    ).rejects.toThrow();
  });
  it("aborts the file transaction on cancellation, budget overflow and a missing ZIP ending", async () => {
    const controller = new AbortController();
    const cancelled = sink();
    await expect(
      receiveTransfer(
        response([Uint8Array.of(1)]),
        "download",
        controller.signal,
        () => controller.abort(),
        cancelled,
      ),
    ).rejects.toThrow("cancelled");
    expect(cancelled.abort).toHaveBeenCalled();
    expect(cancelled.close).not.toHaveBeenCalled();
    const exceeded = sink();
    await expect(
      receiveTransfer(
        response([new Uint8Array(4)], 4, 3),
        "download",
        new AbortController().signal,
        () => {},
        exceeded,
      ),
    ).rejects.toThrow("budget");
    expect(exceeded.write).not.toHaveBeenCalled();
    expect(exceeded.abort).toHaveBeenCalled();
    const incomplete = sink();
    await expect(
      receiveTransfer(
        response([new Uint8Array(64)]),
        "download",
        new AbortController().signal,
        () => {},
        incomplete,
      ),
    ).rejects.toThrow("completion record");
    expect(incomplete.close).not.toHaveBeenCalled();
  });
  it("streams into a file sink and commits only after the complete ZIP footer", async () => {
    const ending = new Uint8Array(22);
    ending.set([0x50, 0x4b, 5, 6]);
    const output = sink();
    const chunks = [
      new Uint8Array(100),
      ending.subarray(0, 7),
      ending.subarray(7),
    ];
    expect(
      await receiveTransfer(
        response(chunks, 100),
        "download",
        new AbortController().signal,
        () => {},
        output,
      ),
    ).toBeNull();
    expect(output.write).toHaveBeenCalledTimes(3);
    expect(output.close).toHaveBeenCalledTimes(1);
    expect(output.abort).not.toHaveBeenCalled();
  });
  it("reserves one transfer independently of each mounted body and releases idempotently", () => {
    const release = reserveTransfer();
    expect(() => reserveTransfer()).toThrow("current");
    release();
    release();
    const next = reserveTransfer();
    next();
  });
});
