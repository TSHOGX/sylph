import { consumeEvidenceFrames } from "./trace-evidence-stream";
import type { TransferSink } from "./trace-transfer";

/** Commit a download only after the common stream validator accepts its footer. */
export async function receiveEvidenceExport(
  response: Response,
  signal: AbortSignal,
  progress: (bytes: number) => void,
  sink?: TransferSink,
): Promise<Blob | null> {
  const chunks: ArrayBuffer[] = [];
  await consumeEvidenceFrames(response, signal, {
    progress,
    finish: () => sink?.close() ?? Promise.resolve(),
    abort: () => sink?.abort() ?? Promise.resolve(),
    chunk: async (bytes) => {
      if (sink) await sink.write(bytes);
      else chunks.push(Uint8Array.from(bytes).buffer);
    },
  });
  signal.throwIfAborted();
  if (sink) return null;
  return new Blob(chunks, { type: "application/x-ndjson" });
}
