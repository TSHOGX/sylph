import { afterEach, expect, it, vi } from "vitest";
import { createHash } from "node:crypto";
import {
  PlaybackClock,
  PLAYBACK_PREVIEW_BYTES,
  receiveEvidencePlayback,
  type PlaybackRow,
} from "./trace-playback";

function stream(
  bodies: Buffer[],
  delays: number[],
  duration = 300_000,
  corrupt = false,
  offsets: number[] = [],
) {
  const frames: Record<string, unknown>[] = [
    {
      kind: "header",
      format: "sylph-evidence-jsonl",
      scope: "environment",
      pacing: { clock: "server_admission", speed: 1, maxGapMs: 2000 },
    },
  ];
  for (let index = 0; index < bodies.length; index++) {
    const payload = `payload-${index}`;
    frames.push(
      {
        kind: "timing",
        admissionSeq: index + 1,
        recordedDeltaMs: index ? delays[index] : null,
        delayMs: delays[index],
        adjustment: index ? "recorded" : "first",
      },
      {
        kind: "capture",
        metadataText: {
          observationRef: JSON.stringify({ producerSeq: index + 1 }),
          sourcePosition: '{"generation":0}',
          capturedAt: '{"value":999}',
          sourceClock: '{"value":1700000000000000001,"unit":"nanoseconds"}',
        },
        record: {
          admissionSeq: index + 1,
          admittedAtMs: index * 100,
          capture: {
            payloadRef: payload,
            observationRef: { producerSeq: index + 1 },
            sourcePosition: { generation: 0 },
            transportKind: "native_file",
            capturedAt: { value: 999 },
            sourceClock: { value: "__exact_clock__", unit: "nanoseconds" },
          },
        },
      },
      {
        kind: "bytes",
        payload,
        offset: offsets[index] ?? 0,
        data: bodies[index].toString("base64"),
      },
      { kind: "payload_end", payload, observedBytes: bodies[index].length },
    );
  }
  const prefix = Buffer.from(
    frames
      .map((frame) => JSON.stringify(frame) + "\n")
      .join("")
      .replaceAll('"__exact_clock__"', "1700000000000000001"),
  );
  const footer =
    JSON.stringify({
      kind: "complete",
      observations: bodies.length,
      frames: frames.length,
      prefixBytes: prefix.length,
      sha256: corrupt
        ? "bad"
        : createHash("sha256").update(prefix).digest("hex"),
    }) + "\n";
  return new Response(Buffer.concat([prefix, Buffer.from(footer)]), {
    headers: {
      "x-sylph-export-limit": "1048576",
      "x-sylph-export-frame-limit": "65536",
      "x-sylph-export-duration-ms": String(duration),
    },
  });
}
afterEach(() => vi.useRealTimers());
it("paces each observed revision and pause preserves the unspent receipt gap", async () => {
  vi.useFakeTimers();
  const rows: PlaybackRow[] = [],
    clock = new PlaybackClock();
  const completion = receiveEvidencePlayback(
    stream(
      ["A", "A", "B", "BC"].map((v) => Buffer.from(v)),
      [0, 100, 0, 20],
    ),
    new AbortController().signal,
    clock,
    () => {},
    (row) => rows.push(row),
    () => {},
  );
  await vi.advanceTimersByTimeAsync(40);
  expect(rows.map((row) => row.preview)).toEqual(["A"]);
  clock.setPaused(true);
  await vi.advanceTimersByTimeAsync(5000);
  expect(rows.map((row) => row.preview)).toEqual(["A"]);
  clock.setPaused(false);
  await vi.advanceTimersByTimeAsync(59);
  expect(rows.map((row) => row.preview)).toEqual(["A"]);
  await vi.advanceTimersByTimeAsync(1);
  expect(rows.map((row) => row.preview)).toEqual(["A", "A", "B"]);
  await vi.advanceTimersByTimeAsync(21);
  await completion;
  expect(rows.map((row) => row.preview)).toEqual(["A", "A", "B", "BC"]);
  expect(rows.map((row) => row.admission)).toEqual([1, 2, 3, 4]);
});
it("keeps a bounded lexical preview and labels binary prefixes without loading a whole body", async () => {
  const rows: PlaybackRow[] = [];
  await receiveEvidencePlayback(
    stream([Buffer.from("中".repeat(6000)), Buffer.from([0, 255, 1])], [0, 0]),
    new AbortController().signal,
    new PlaybackClock(),
    () => {},
    (row) => rows.push(row),
    () => {},
  );
  expect(rows[0].preview).toBe(
    "中".repeat(Math.floor(PLAYBACK_PREVIEW_BYTES / 3)),
  );
  expect(rows[0].previewBytes).toBe(PLAYBACK_PREVIEW_BYTES);
  expect(rows[0].observedBytes).toBe(18000);
  expect(rows[1].encoding).toBe("base64");
  expect(rows[1].preview).toBe("AP8B");
});
it("cancellation and lifetime expiry interrupt a paused player instead of waiting for resume", async () => {
  vi.useFakeTimers();
  for (const expired of [false, true]) {
    const clock = new PlaybackClock(),
      abort = new AbortController();
    clock.setPaused(true);
    const completion = receiveEvidencePlayback(
      stream([Buffer.from("A")], [0], 100),
      abort.signal,
      clock,
      () => {},
      () => {
        throw new Error("Paused playback emitted a row");
      },
      () => {},
    );
    const rejected = expect(completion).rejects.toMatchObject({
      name: expired ? "TimeoutError" : "AbortError",
    });
    if (!expired) abort.abort();
    await vi.advanceTimersByTimeAsync(101);
    await rejected;
  }
});
it("a provisional playback cannot report completion when its digest is corrupt", async () => {
  await expect(
    receiveEvidencePlayback(
      stream([Buffer.from("A")], [0], 1000, true),
      new AbortController().signal,
      new PlaybackClock(),
      () => {},
      () => {},
      () => {},
    ),
  ).rejects.toThrow(/completion/);
});

it("a sparse payload never relabels a later retained interval as a byte-zero prefix", async () => {
  const rows: PlaybackRow[] = [];
  await receiveEvidencePlayback(
    stream([Buffer.from("PUBLIC")], [0], 1000, false, [5]),
    new AbortController().signal,
    new PlaybackClock(),
    () => {},
    (row) => rows.push(row),
    () => {},
  );
  expect(rows[0].missingPrefix).toBe(true);
  expect(rows[0].preview).toBe("");
  expect(rows[0].previewBytes).toBe(0);
  expect(rows[0].observedBytes).toBe(6);
});

it("clock metadata keeps exact integer digits beyond JavaScript numeric precision", async () => {
  const rows: PlaybackRow[] = [];
  await receiveEvidencePlayback(
    stream([Buffer.from("A")], [0]),
    new AbortController().signal,
    new PlaybackClock(),
    () => {},
    (row) => rows.push(row),
    () => {},
  );
  expect(rows[0].sourceClock).toBe(
    '{"value":1700000000000000001,"unit":"nanoseconds"}',
  );
});
