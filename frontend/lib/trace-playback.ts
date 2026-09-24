import { consumeEvidenceFrames } from "./trace-evidence-stream";

export const PLAYBACK_WINDOW = 64;
export const PLAYBACK_PREVIEW_BYTES = 1024;
export interface PlaybackPolicy {
  clock: "server_admission";
  speed: number;
  maxGapMs: number;
}
export interface PlaybackRow {
  admission: number;
  receivedAtMs: number;
  observation: string;
  source: string;
  transport: string;
  capturedAt: string;
  sourceClock: string;
  recordedDeltaMs: number | null;
  delayMs: number;
  adjustment: string;
  preview: string;
  encoding: "utf8" | "base64";
  previewBytes: number;
  observedBytes: number;
  missingPrefix: boolean;
}

/** Pause wakes the current wait and preserves only its unspent running time. */
export class PlaybackClock {
  private paused = false;
  private wake = new Set<() => void>();
  setPaused(paused: boolean) {
    this.paused = paused;
    for (const wake of this.wake) wake();
  }
  async wait(milliseconds: number, signal: AbortSignal) {
    let remaining = milliseconds;
    signal.throwIfAborted();
    while (this.paused || remaining > 0) {
      const running = !this.paused,
        started = performance.now();
      await new Promise<void>((resolve, reject) => {
        const finish = () => {
          cleanup();
          resolve();
        };
        const abort = () => {
          cleanup();
          reject(signal.reason);
        };
        const timer = running ? setTimeout(finish, remaining) : undefined;
        const cleanup = () => {
          clearTimeout(timer);
          this.wake.delete(finish);
          signal.removeEventListener("abort", abort);
        };
        this.wake.add(finish);
        signal.addEventListener("abort", abort, { once: true });
      });
      signal.throwIfAborted();
      if (running)
        remaining = Math.max(0, remaining - (performance.now() - started));
    }
  }
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("Invalid playback metadata");
  return value as Record<string, unknown>;
}
function integer(value: unknown, minimum = 0): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < minimum
  )
    throw new Error("Invalid playback quantity");
  return value;
}
function label(value: unknown) {
  if (typeof value !== "string")
    throw new Error("Playback omitted exact metadata text");
  return value.length > 512 ? value.slice(0, 512) + "…" : value;
}

/** Retain one raw prefix while consuming every exact frame; never interpret vendor JSON. */
export async function receiveEvidencePlayback(
  response: Response,
  signal: AbortSignal,
  clock: PlaybackClock,
  onPolicy: (policy: PlaybackPolicy) => void,
  onRow: (row: PlaybackRow) => void,
  progress: (bytes: number) => void,
): Promise<void> {
  const duration = Number(response.headers.get("x-sylph-export-duration-ms"));
  if (!Number.isSafeInteger(duration) || duration <= 0 || duration > 300_000)
    throw new Error("Invalid playback lifetime");
  const deadline = new AbortController();
  const timeout = setTimeout(
    () =>
      deadline.abort(
        new DOMException("Playback duration exceeded", "TimeoutError"),
      ),
    duration,
  );
  const active = AbortSignal.any([signal, deadline.signal]);
  let policy: PlaybackPolicy | null = null;
  let timing: Record<string, unknown> | null = null;
  let current: {
    row: PlaybackRow;
    payload: string;
    prefix: Uint8Array;
    size: number;
    next: number;
    emitted: boolean;
  } | null = null;
  try {
    await consumeEvidenceFrames(response, active, {
      timing: true,
      progress,
      frame: async (frame) => {
        await clock.wait(0, active);
        if (frame.kind === "header") {
          const pacing = object(frame.pacing);
          if (
            pacing.clock !== "server_admission" ||
            ![1, 2, 4, 10].includes(integer(pacing.speed)) ||
            integer(pacing.maxGapMs, 1) > 2000
          )
            throw new Error("Unsupported playback pacing");
          policy = {
            clock: "server_admission",
            speed: Number(pacing.speed),
            maxGapMs: Number(pacing.maxGapMs),
          };
          onPolicy(policy);
        } else if (frame.kind === "timing") {
          if (!policy || timing || (current && !current.emitted))
            throw new Error("Playback observation is incomplete");
          const delay = integer(frame.delayMs);
          if (
            delay > policy.maxGapMs ||
            ![
              "first",
              "recorded",
              "gap_clamped",
              "backward_clock",
              "unrepresentable_delta",
            ].includes(String(frame.adjustment))
          )
            throw new Error("Invalid playback gap");
          integer(frame.admissionSeq, 1);
          if (frame.recordedDeltaMs !== null)
            integer(frame.recordedDeltaMs, Number.MIN_SAFE_INTEGER);
          timing = frame;
          await clock.wait(delay, active);
        } else if (frame.kind === "capture") {
          const record = object(frame.record),
            capture = object(record.capture),
            metadata = object(frame.metadataText);
          if (
            !timing ||
            timing.admissionSeq !== record.admissionSeq ||
            typeof capture.payloadRef !== "string"
          )
            throw new Error("Playback capture has no matching receipt timing");
          current = {
            row: {
              admission: integer(record.admissionSeq, 1),
              receivedAtMs: integer(
                record.admittedAtMs,
                Number.MIN_SAFE_INTEGER,
              ),
              observation: label(metadata.observationRef),
              source: label(metadata.sourcePosition),
              transport: String(capture.transportKind).slice(0, 128),
              capturedAt: label(metadata.capturedAt),
              sourceClock: label(metadata.sourceClock),
              recordedDeltaMs: timing.recordedDeltaMs as number | null,
              delayMs: timing.delayMs as number,
              adjustment: String(timing.adjustment),
              preview: "",
              encoding: "utf8",
              previewBytes: 0,
              observedBytes: 0,
              missingPrefix: false,
            },
            payload: capture.payloadRef,
            prefix: new Uint8Array(PLAYBACK_PREVIEW_BYTES),
            size: 0,
            next: 0,
            emitted: false,
          };
          timing = null;
        } else if (
          frame.kind === "bytes" &&
          current &&
          frame.payload === current.payload
        ) {
          const offset = integer(frame.offset);
          if (offset !== current.next && current.size < PLAYBACK_PREVIEW_BYTES)
            current.row.missingPrefix = true;
          if (
            !current.row.missingPrefix &&
            current.size < PLAYBACK_PREVIEW_BYTES
          ) {
            if (typeof frame.data !== "string")
              throw new Error("Invalid playback payload bytes");
            const needed = PLAYBACK_PREVIEW_BYTES - current.size;
            const prefix = atob(frame.data.slice(0, Math.ceil(needed / 3) * 4));
            const length = Math.min(needed, prefix.length);
            for (let index = 0; index < length; index++)
              current.prefix[current.size + index] = prefix.charCodeAt(index);
            current.size += length;
          }
          // Base64 length carries the exact decoded size without a body-sized copy.
          if (typeof frame.data !== "string" || frame.data.length % 4 !== 0)
            throw new Error("Invalid playback payload bytes");
          current.next =
            offset +
            (frame.data.length / 4) * 3 -
            (frame.data.endsWith("==") ? 2 : frame.data.endsWith("=") ? 1 : 0);
        } else if (
          frame.kind === "payload_end" &&
          current &&
          frame.payload === current.payload
        ) {
          if (current.emitted) throw new Error("Playback payload ended twice");
          const bytes = current.prefix.subarray(0, current.size);
          current.row.observedBytes = integer(frame.observedBytes);
          current.row.previewBytes = current.size;
          try {
            if (bytes.includes(0)) throw new Error("Binary prefix");
            current.row.preview = new TextDecoder("utf-8", {
              fatal: true,
              ignoreBOM: true,
            }).decode(bytes, {
              stream: current.size < current.row.observedBytes,
            });
          } catch {
            current.row.encoding = "base64";
            current.row.preview = btoa(String.fromCharCode(...bytes));
          }
          current.emitted = true;
          onRow(current.row);
        } else if (
          frame.kind === "complete" &&
          (timing || (current && !current.emitted))
        ) {
          throw new Error("Playback ended inside an observation");
        }
      },
    });
  } finally {
    clearTimeout(timeout);
  }
}
