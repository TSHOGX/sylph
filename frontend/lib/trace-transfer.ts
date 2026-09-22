/** Explicit transfer buffers are independent of retained reader caches. */
export interface TransferSink {
  write(bytes: Uint8Array): Promise<void>;
  close(): Promise<void>;
  abort(): Promise<void>;
}
let active = false;
export function reserveTransfer(): () => void {
  if (active)
    throw new Error(
      "Finish or cancel the current observed-content transfer first.",
    );
  active = true;
  let released = false;
  return () => {
    if (!released) {
      released = true;
      active = false;
    }
  };
}
function quantity(response: Response, name: string): number {
  const text = response.headers.get(name);
  if (text === null || !/^\d+$/.test(text))
    throw new Error("Transfer omitted its byte budget or length.");
  const value = Number(text);
  if (!Number.isSafeInteger(value) || value < 0)
    throw new Error("Transfer returned an invalid byte budget or length.");
  return value;
}
/** Validate every byte before committing a file or clipboard. No body enters React. */
export async function receiveTransfer(
  response: Response,
  kind: "copy" | "download",
  signal: AbortSignal,
  progress: (bytes: number, total: number | null) => void,
  sink?: TransferSink,
): Promise<string | Blob | null> {
  const maximum = quantity(response, "x-sylph-transfer-limit");
  const observed = quantity(response, "x-sylph-observed-bytes");
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Transfer body is unavailable.");
  const decoder =
    kind === "copy"
      ? new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
      : null;
  const chunks: ArrayBuffer[] = [];
  const prefix: number[] = [];
  let received = 0;
  let ending = new Uint8Array(0);
  const abort = () => {
    void reader.cancel().catch(() => {});
    void sink?.abort().catch(() => {});
  };
  signal.addEventListener("abort", abort, { once: true });
  try {
    if (signal.aborted)
      throw new DOMException("Transfer cancelled", "AbortError");
    for (;;) {
      const { value, done } = await reader.read();
      if (signal.aborted)
        throw new DOMException("Transfer cancelled", "AbortError");
      if (done) break;
      received += value.byteLength;
      if (kind === "download") {
        const keep = value.subarray(Math.max(0, value.length - 22));
        const tail = new Uint8Array(Math.min(22, ending.length + keep.length));
        const fromOld = tail.length - keep.length;
        if (fromOld > 0) tail.set(ending.subarray(ending.length - fromOld));
        tail.set(keep, fromOld);
        ending = tail;
      }
      if (received > maximum)
        throw new Error("Transfer exceeded its advertised byte budget.");
      for (let start = 0; start < value.length; start += 64 * 1024) {
        const part = value.subarray(
          start,
          Math.min(start + 64 * 1024, value.length),
        );
        for (const byte of part.subarray(0, Math.max(0, 3 - prefix.length)))
          prefix.push(byte);
        decoder?.decode(part, { stream: true });
        if (sink) await sink.write(part);
        else chunks.push(part.slice().buffer);
      }
      progress(received, kind === "copy" ? observed : null);
      // A buffered response can resolve every read as a microtask. Yield so
      // cancellation and other reader input still progress during an explicit job.
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    }
    decoder?.decode();
    if (kind === "copy" && received !== observed)
      throw new Error("Transfer ended before all observed bytes arrived.");
    // Sylph's stored ZIP stream has no archive comment. A missing completion
    // record must not commit a partial file even if a proxy ended its body cleanly.
    if (
      kind === "download" &&
      (received < observed ||
        ending.length !== 22 ||
        ending[0] !== 0x50 ||
        ending[1] !== 0x4b ||
        ending[2] !== 5 ||
        ending[3] !== 6 ||
        ending[20] !== 0 ||
        ending[21] !== 0)
    ) {
      throw new Error("Archive transfer ended before its completion record.");
    }
    if (signal.aborted)
      throw new DOMException("Transfer cancelled", "AbortError");
    if (sink) {
      await sink.close();
      return null;
    }
    const blob = new Blob(chunks, {
      type: kind === "copy" ? "text/plain;charset=utf-8" : "application/zip",
    });
    if (kind === "download") return blob;
    // Blob.text decodes off the synchronous rendering path and strips one UTF-8
    // BOM. Restore that observed scalar; validation above has rejected bad UTF-8.
    const text = await blob.text();
    if (signal.aborted)
      throw new DOMException("Transfer cancelled", "AbortError");
    return (
      (prefix[0] === 0xef && prefix[1] === 0xbb && prefix[2] === 0xbf
        ? "\ufeff"
        : "") + text
    );
  } catch (error) {
    await reader.cancel().catch(() => {});
    await sink?.abort().catch(() => {});
    throw error;
  } finally {
    signal.removeEventListener("abort", abort);
    reader.releaseLock();
  }
}
