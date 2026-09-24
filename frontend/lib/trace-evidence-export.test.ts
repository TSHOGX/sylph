import { expect, it, vi } from "vitest";
import { createHash } from "node:crypto";
import { receiveEvidenceExport } from "./trace-evidence-export";

function archive() {
  const prefix = Buffer.from(
    [
      JSON.stringify({
        kind: "header",
        format: "sylph-evidence-jsonl",
        scope: "environment",
        environment: "env",
      }),
      JSON.stringify({ kind: "capture", record: { unknown: "中文" } }),
    ].join("\n") + "\n",
  );
  return Buffer.concat([
    prefix,
    Buffer.from(
      JSON.stringify({
        kind: "complete",
        prefixBytes: prefix.length,
        frames: 2,
        observations: 1,
        sha256: createHash("sha256").update(prefix).digest("hex"),
      }) + "\n",
    ),
  ]);
}
function response(bytes: Uint8Array, maximum = 1024 * 1024) {
  return new Response(
    new ReadableStream({
      start(controller) {
        for (let i = 0; i < bytes.length; i += 7)
          controller.enqueue(bytes.slice(i, i + 7));
        controller.close();
      },
    }),
    {
      headers: {
        "x-sylph-export-limit": String(maximum),
        "x-sylph-export-frame-limit": "1024",
      },
    },
  );
}
it("commits exact streamed archive bytes only after validating the completion digest", async () => {
  const bytes = archive();
  const blob = await receiveEvidenceExport(
    response(bytes),
    new AbortController().signal,
    () => {},
  );
  expect(new Uint8Array(await blob!.arrayBuffer())).toEqual(
    new Uint8Array(bytes),
  );
});
it("rejects clean truncation, corrupted content, mismatched counts and trailing data before file commit", async () => {
  const valid = archive();
  const text = valid.toString();
  for (const bytes of [
    valid.subarray(0, valid.indexOf('{"kind":"complete"')),
    Buffer.from(text.replace("中文", "文字")),
    Buffer.from(text.replace('"observations":1', '"observations":9')),
    Buffer.from(text + "{}\n"),
  ]) {
    const sink = {
      write: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
      abort: vi.fn(async () => {}),
    };
    await expect(
      receiveEvidenceExport(
        response(bytes),
        new AbortController().signal,
        () => {},
        sink,
      ),
    ).rejects.toThrow();
    expect(sink.close).not.toHaveBeenCalled();
    expect(sink.abort).toHaveBeenCalled();
  }
});
it("enforces output and frame bounds independently of stream chunk size", async () => {
  await expect(
    receiveEvidenceExport(
      response(archive(), 20),
      new AbortController().signal,
      () => {},
    ),
  ).rejects.toThrow(/budget/);
  await expect(
    receiveEvidenceExport(
      response(Buffer.from("a".repeat(1025))),
      new AbortController().signal,
      () => {},
    ),
  ).rejects.toThrow(/frame/);
});
it("cancellation aborts the transactional file instead of committing partial evidence", async () => {
  const controller = new AbortController();
  const sink = {
    write: vi.fn(async () => {
      controller.abort();
    }),
    close: vi.fn(async () => {}),
    abort: vi.fn(async () => {}),
  };
  await expect(
    receiveEvidenceExport(
      response(archive()),
      controller.signal,
      () => {},
      sink,
    ),
  ).rejects.toThrow(/abort|cancel/i);
  expect(sink.close).not.toHaveBeenCalled();
  expect(sink.abort).toHaveBeenCalled();
});

it("cancellation remains attached while a verified file is waiting to commit", async () => {
  const controller = new AbortController();
  let entered!: () => void, release!: () => void;
  const closing = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const sink = {
    write: vi.fn(async () => {}),
    close: vi.fn(async () => {
      entered();
      await held;
    }),
    abort: vi.fn(async () => {}),
  };
  const result = receiveEvidenceExport(
    response(archive()),
    controller.signal,
    () => {},
    sink,
  ).then(
    () => false,
    (error) => error instanceof DOMException && error.name === "AbortError",
  );
  await closing;
  controller.abort();
  const abortedBeforeCommit = sink.abort.mock.calls.length > 0;
  release();
  expect(await result).toBe(true);
  expect(abortedBeforeCommit).toBe(true);
});
