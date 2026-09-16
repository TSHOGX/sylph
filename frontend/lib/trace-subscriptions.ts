import { createSSEDecoder } from "./sse-parse";
import {
  clearTraceAccess,
  invalidateTraceView,
  onTraceAccessCleared,
} from "./trace-cache";
import type { TraceChange, TraceLimits, TraceView } from "./trace-types";

type Listener = (change: TraceChange) => void;
const interests = new Map<string, Map<Listener, TraceView>>();
let controller: AbortController | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;
let cursor: string | null = null;
let checkpoint: Pick<TraceView, "generation" | "viewRevision"> | null = null;
let resetViews = new Set<string>();
let attempts = 0;
let maximum = 0;
let maximumBytes = 0;
let maximumChanges = 0;
function stop() {
  controller?.abort();
  controller = null;
  clearTimeout(timer);
}
function proofs() {
  const unique = new Map<string, { agent: string; atSnapshot: string }>();
  for (const [agent, readers] of interests)
    for (const view of readers.values())
      unique.set(JSON.stringify([agent, view.token]), {
        agent,
        atSnapshot: view.token,
      });
  return [...unique.values()];
}
function publish(change: TraceChange) {
  const listeners: Listener[] = [];
  for (const [agent, readers] of interests)
    if (change.kind !== "revision" || change.agents.includes(agent))
      listeners.push(...readers.keys());
  // A listener can replace itself synchronously. That new reader has a bootstrap
  // after this notification, and must not receive the old reader's reset again.
  listeners.forEach((listener) => listener(change));
}
function requireBootstrap(
  change: Extract<TraceChange, { kind: "reset_required" }>,
) {
  resetViews = new Set(proofs().map((proof) => proof.atSnapshot));
  cursor = null;
  checkpoint = null;
  attempts = 0;
  // Stop the rejected stream before delivering callbacks: a callback may install
  // a fresh bootstrap immediately, and its connection belongs to that new view.
  stop();
  publish(change);
}
function connect() {
  stop();
  if (!interests.size || !cursor) return;
  const current = new AbortController();
  controller = current;
  const query = new URLSearchParams({
    interests: JSON.stringify([...interests.keys()]),
    views: JSON.stringify(proofs()),
    cursor,
  });
  void (async () => {
    try {
      const response = await fetch(`/api/v1/trace/changes?${query}`, {
        signal: current.signal,
        headers: { Accept: "text/event-stream" },
        credentials: "include",
        cache: "no-store",
      });
      if (current.signal.aborted) return;
      if (response.status === 401 || response.status === 403) {
        clearTraceAccess();
        return;
      }
      // A 410 can mean a revoked deletion epoch as well as expiration. Clear
      // retained content, preserve local drafts, and acquire fresh around-anchor views.
      if (response.status === 410) {
        invalidateTraceView();
        return;
      }
      if (!response.ok || !response.body)
        throw new Error(`Trace subscription ${response.status}`);
      const reader = response.body.getReader();
      const utf8 = new TextDecoder();
      const decoder = createSSEDecoder(maximumBytes);
      try {
        for (;;) {
          const chunk = await reader.read();
          if (chunk.done) break;
          for (const frame of decoder.push(
            utf8.decode(chunk.value, { stream: true }),
          )) {
            if (current.signal.aborted) return;
            if (!frame.data) continue;
            const change = JSON.parse(frame.data) as TraceChange;
            attempts = 0;
            if (
              change.kind === "revision" &&
              change.changed.length +
                change.removed.length +
                change.moved.length >
                maximumChanges
            )
              throw new Error("Trace change exceeded its item bound");
            if (change.kind === "access_changed") {
              clearTraceAccess();
              return;
            }
            if (change.kind === "reset_required") {
              requireBootstrap(change);
              return;
            }
            if (change.kind === "revision") {
              if (change.cursor === cursor) continue;
              cursor = change.cursor;
              checkpoint = change;
            }
            publish(change);
          }
        }
      } finally {
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
    } catch (error) {
      if (!current.signal.aborted)
        publish({
          kind: "reset_required",
          reason:
            error instanceof Error
              ? error.message
              : "Trace connection interrupted",
        });
    }
    // EOF is connectivity, never execution settlement. Commands are never replayed.
    if (!current.signal.aborted && interests.size)
      timer = setTimeout(
        connect,
        [500, 1000, 2000, 5000][Math.min(attempts++, 3)],
      );
  })();
}
export function subscribeTrace(
  agent: string,
  view: TraceView,
  after: string,
  limits: TraceLimits,
  listener: Listener,
): () => void {
  maximum = limits.maxVisibleInterests;
  maximumBytes = limits.maxSummaryBytes;
  maximumChanges = limits.maxChangeItems;
  if (
    checkpoint &&
    checkpoint.generation !== view.generation &&
    !resetViews.has(view.token)
  ) {
    requireBootstrap({
      kind: "reset_required",
      reason: "Trace generation changed; refresh this retained view.",
    });
  }
  const readers = interests.get(agent) ?? new Map<Listener, TraceView>();
  readers.set(listener, view);
  interests.set(agent, readers);
  if (proofs().length > maximum) {
    readers.delete(listener);
    if (!readers.size) interests.delete(agent);
    throw new Error("Close another reader before observing more agents.");
  }
  // A newer bootstrap must not skip changes still owed to an existing reader.
  // A reader with an older checkpoint rewinds replay; duplicate changes are harmless.
  if (
    !resetViews.has(view.token) &&
    (!cursor ||
      (checkpoint?.generation === view.generation &&
        view.viewRevision < checkpoint.viewRevision))
  ) {
    cursor = after;
    checkpoint = view;
  }
  connect();
  return () => {
    readers.delete(listener);
    if (!readers.size) interests.delete(agent);
    if (!interests.size) {
      cursor = null;
      checkpoint = null;
      attempts = 0;
    }
    connect();
  };
}
onTraceAccessCleared(() => {
  stop();
  interests.clear();
  cursor = null;
  checkpoint = null;
  resetViews.clear();
  attempts = 0;
});
