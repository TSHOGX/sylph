import { ApiError } from "./api";

/** Retry only reads whose server reports unfinished projection or indexing work. */
export async function readTraceWhenReady<T>(
  read: () => Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    signal.throwIfAborted();
    try {
      return await read();
    } catch (error) {
      if (
        attempt >= 7 ||
        !(error instanceof ApiError) ||
        error.status !== 503 ||
        !["projection_pending", "search_indexing"].includes(error.code)
      )
        throw error;
      await new Promise<void>((resolve, reject) => {
        const abort = () => {
          clearTimeout(timer);
          reject(signal.reason);
        };
        const timer = setTimeout(
          () => {
            signal.removeEventListener("abort", abort);
            resolve();
          },
          Math.min(250 * 2 ** attempt, 5000),
        );
        signal.addEventListener("abort", abort, { once: true });
        if (signal.aborted) abort();
      });
    }
  }
}
