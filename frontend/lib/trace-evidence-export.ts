import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import type { TransferSink } from "./trace-transfer";

/** A bounded line buffer and incremental digest certify completion before file commit. */
export async function receiveEvidenceExport(
  response: Response,
  signal: AbortSignal,
  progress: (bytes: number) => void,
  sink?: TransferSink,
): Promise<Blob | null> {
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
    hash = sha256.create(),
    chunks: ArrayBuffer[] = [];
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let filled = 0,
    received = 0,
    prefixBytes = 0,
    frames = 0,
    observations = 0,
    complete = false;
  const abort = () => {
    void reader.cancel().catch(() => {});
    void sink?.abort().catch(() => {});
  };
  signal.addEventListener("abort", abort, { once: true });
  try {
    if (signal.aborted)
      throw new DOMException("Export cancelled", "AbortError");
    for (;;) {
      const { value, done } = await reader.read();
      if (signal.aborted)
        throw new DOMException("Export cancelled", "AbortError");
      if (done) break;
      received += value.byteLength;
      if (received > maximum)
        throw new Error("Evidence export exceeded its output budget");
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
            frame = JSON.parse(decoder.decode(bytes));
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
              ].includes(frame.kind) ||
              (frames > 0 && frame.kind === "header")
            )
              throw new Error("Invalid evidence export frame");
            if (frame.kind === "capture") observations++;
            hash.update(bytes);
            prefixBytes += filled;
            frames++;
          }
          filled = 0;
        }
        validated += size;
        if (validated >= 64 * 1024) {
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
          if (signal.aborted)
            throw new DOMException("Export cancelled", "AbortError");
          validated = 0;
        }
      }
      for (let offset = 0; offset < value.length; offset += 64 * 1024) {
        const bytes = value.subarray(
          offset,
          Math.min(offset + 64 * 1024, value.length),
        );
        if (sink) await sink.write(bytes);
        else chunks.push(Uint8Array.from(bytes).buffer);
      }
      progress(received);
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    }
    if (!complete || filled !== 0)
      throw new Error("Evidence export ended without verified completion");
    if (signal.aborted)
      throw new DOMException("Export cancelled", "AbortError");
    if (sink) {
      await sink.close();
      return null;
    }
    return new Blob(chunks, { type: "application/x-ndjson" });
  } catch (error) {
    await reader.cancel().catch(() => {});
    await sink?.abort().catch(() => {});
    throw error;
  } finally {
    signal.removeEventListener("abort", abort);
    reader.releaseLock();
  }
}
