import { createSSEDecoder, type SSEFrame } from "@/lib/sse-parse";

const RETRY_DELAYS = [500, 1000, 2000, 5000, 10000];

/** Notifications are hints without replay; each recovery reconciles the resources. */
export function subscribeSessionNotifications(
  url: string,
  onFrame: (frame: SSEFrame) => void,
  onRecovered: () => void,
): () => void {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let failures = 0;
  let attempted = false;

  async function connect() {
    if (controller.signal.aborted) return;
    const recovering = attempted;
    attempted = true;
    try {
      const response = await fetch(url, {
        signal: controller.signal,
        headers: { Accept: "text/event-stream" },
        credentials: "include",
        cache: "no-store",
      });
      if (!response.ok || !response.body) throw new Error(`Notification stream: ${response.status}`);
      if (recovering) onRecovered();
      const reader = response.body.getReader();
      const utf8 = new TextDecoder();
      const frames = createSSEDecoder();
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done || controller.signal.aborted) break;
          // Reset only after traffic: a server immediately closing every HTTP 200
          // must still back off, rather than retrying twice a second forever.
          failures = 0;
          for (const frame of frames.push(utf8.decode(value, { stream: true }))) onFrame(frame);
        }
      } finally {
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
    } catch {
      if (controller.signal.aborted) return;
    }
    if (!controller.signal.aborted) {
      timer = setTimeout(() => void connect(), RETRY_DELAYS[Math.min(failures++, RETRY_DELAYS.length - 1)]);
    }
  }

  void connect();
  return () => {
    controller.abort();
    clearTimeout(timer);
  };
}
