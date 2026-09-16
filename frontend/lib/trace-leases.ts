import type { TraceView } from "./trace-types";

interface Hold {
  readers: number;
  releaseTimer: ReturnType<typeof setTimeout> | null;
}
const leases = new Map<string, Hold>();
let departing = false;
let evict: (token: string) => void = () => {};
export function onTraceViewReleased(callback: (token: string) => void) {
  evict = callback;
}
/** Release is delayed one task so Strict Mode probes and navigation handoffs retain ownership. */
export function retainTraceView(view: TraceView): () => void {
  const hold = leases.get(view.token) ?? { readers: 0, releaseTimer: null };
  if (hold.releaseTimer !== null) clearTimeout(hold.releaseTimer);
  hold.releaseTimer = null;
  hold.readers++;
  leases.set(view.token, hold);
  if (departing) releaseView(view.token, hold);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    hold.readers--;
    if (hold.readers === 0 && leases.get(view.token) === hold)
      scheduleRelease(view.token, hold);
  };
}
function scheduleRelease(token: string, hold: Hold) {
  if (hold.releaseTimer !== null) return;
  hold.releaseTimer = setTimeout(() => {
    if (hold.readers || leases.get(token) !== hold) return;
    releaseView(token, hold);
  }, 0);
}
function releaseView(token: string, hold: Hold) {
  if (leases.get(token) !== hold) return;
  if (hold.releaseTimer !== null) clearTimeout(hold.releaseTimer);
  leases.delete(token);
  evict(token);
  void fetch(`/api/v1/trace/views/${encodeURIComponent(token)}`, {
    method: "DELETE",
    credentials: "include",
    cache: "no-store",
    keepalive: true,
  }).catch(() => {});
}
export function discardUnusedTraceView(view: TraceView) {
  if (!leases.has(view.token)) {
    const hold = { readers: 0, releaseTimer: null };
    leases.set(view.token, hold);
    if (departing) releaseView(view.token, hold);
    else scheduleRelease(view.token, hold);
  }
}
export function viewOfTraceResponse(value: unknown): TraceView | null {
  if (!value || typeof value !== "object" || !("view" in value)) return null;
  const view = value.view;
  if (
    !view ||
    typeof view !== "object" ||
    !("token" in view) ||
    typeof view.token !== "string"
  )
    return null;
  return view as TraceView;
}
export function clearTraceLeases() {
  for (const [token, hold] of leases) releaseView(token, hold);
}

// A document navigation does not unmount React, and a frozen/discarded document
// may never run a queued release. Submit keepalive DELETEs inside pagehide itself.
// Leases still expire on the server when a process dies without lifecycle events.
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", () => {
    departing = true;
    clearTraceLeases();
  });
  window.addEventListener("pageshow", () => {
    departing = false;
  });
}
