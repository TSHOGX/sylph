import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";

export interface EvidenceConsumer {
  timing?: boolean;
  frame?: (frame: Record<string, unknown>) => Promise<void>;
  chunk?: (bytes: Uint8Array) => Promise<void>;
  progress?: (bytes: number) => void;
  abort?: () => Promise<void>;
  finish?: () => Promise<void>;
}

/** One bounded framing/digest validator serves downloads and paced consumption. */
export async function consumeEvidenceFrames(
  response: Response,
  signal: AbortSignal,
  consumer: EvidenceConsumer,
): Promise<void> {
  const budget = (name: string) => {
    const text = response.headers.get(name);
    if (!text || !/^\d+$/.test(text))
      throw new Error("Export omitted its byte budget");
    const value = Number(text);
    if (!Number.isSafeInteger(value) || value <= 0)
      throw new Error("Invalid export byte budget");
    return value;
  };
  const maximum = budget("x-sylph-export-limit"),
    frameMaximum = budget("x-sylph-export-frame-limit");
  if (frameMaximum > 2 * 1024 * 1024)
    throw new Error("Export frame exceeds the browser validation budget");
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Evidence export body is unavailable");
  const line = new Uint8Array(frameMaximum),
    hash = sha256.create();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let filled = 0,
    received = 0,
    prefixBytes = 0,
    frames = 0,
    observations = 0,
    complete = false;
  const abort = () => {
    void reader.cancel().catch(() => {});
    void consumer.abort?.().catch(() => {});
  };
  signal.addEventListener("abort", abort, { once: true });
  try {
    signal.throwIfAborted();
    for (;;) {
      const { value, done } = await reader.read();
      signal.throwIfAborted();
      if (done) break;
      received += value.byteLength;
      if (received > maximum)
        throw new Error("Evidence export exceeded its output budget");
      consumer.progress?.(received);
      let at = 0,
        validated = 0;
      while (at < value.length) {
        if (complete)
          throw new Error("Evidence export has data after completion");
        const newline = value.indexOf(10, at);
        const end = newline < 0 ? value.length : newline + 1;
        const size = end - at;
        if (filled + size > frameMaximum)
          throw new Error("Evidence export frame exceeded its limit");
        line.set(value.subarray(at, end), filled);
        filled += size;
        at = end;
        if (newline >= 0) {
          const bytes = line.subarray(0, filled),
            frame = JSON.parse(decoder.decode(bytes)) as Record<
              string,
              unknown
            >;
          if (!frame || typeof frame !== "object" || Array.isArray(frame))
            throw new Error("Invalid evidence export frame");
          if (
            frames === 0 &&
            (frame.kind !== "header" ||
              frame.format !== "sylph-evidence-jsonl" ||
              frame.scope !== "environment")
          )
            throw new Error("Unknown evidence export format");
          if (frame.kind === "complete") {
            if (
              frame.prefixBytes !== prefixBytes ||
              frame.frames !== frames ||
              frame.observations !== observations ||
              frame.sha256 !== bytesToHex(hash.digest())
            )
              throw new Error(
                "Evidence export completion does not match its bytes",
              );
            complete = true;
          } else {
            if (
              ![
                "header",
                "capture",
                "payload",
                "bytes",
                "payload_end",
                ...(consumer.timing ? ["timing"] : []),
              ].includes(String(frame.kind)) ||
              (frames > 0 && frame.kind === "header")
            )
              throw new Error("Invalid evidence export frame");
            if (frame.kind === "capture") observations++;
            hash.update(bytes);
            prefixBytes += filled;
            frames++;
          }
          await consumer.frame?.(frame);
          filled = 0;
        }
        validated += size;
        if (validated >= 64 * 1024) {
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
          signal.throwIfAborted();
          validated = 0;
        }
      }
      for (let offset = 0; offset < value.length; offset += 64 * 1024) {
        const bytes = value.subarray(
          offset,
          Math.min(offset + 64 * 1024, value.length),
        );
        await consumer.chunk?.(bytes);
      }
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    }
    if (!complete || filled !== 0)
      throw new Error("Evidence export ended without verified completion");
    signal.throwIfAborted();
    // File commit remains inside the cancellation lifetime, even after EOF.
    await consumer.finish?.();
    signal.throwIfAborted();
  } catch (error) {
    await reader.cancel().catch(() => {});
    await consumer.abort?.().catch(() => {});
    throw error;
  } finally {
    signal.removeEventListener("abort", abort);
    reader.releaseLock();
  }
}
