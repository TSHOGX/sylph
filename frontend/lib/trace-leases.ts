import type { TraceView } from "./trace-types";

interface Hold {
  readers: number;
  releaseTimer: ReturnType<typeof setTimeout> | null;
}
const leases = new Map<string, Hold>();
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
  let released = false;
  return () => {
    if (released) return;
    released = true;
    hold.readers--;
    if (hold.readers === 0) scheduleRelease(view.token, hold);
  };
}
function scheduleRelease(token: string, hold: Hold) {
  if (hold.releaseTimer !== null) return;
  hold.releaseTimer = setTimeout(() => {
    if (hold.readers || leases.get(token) !== hold) return;
    leases.delete(token);
    evict(token);
    void fetch(`/api/v1/trace/views/${encodeURIComponent(token)}`, {
      method: "DELETE",
      credentials: "include",
      cache: "no-store",
      keepalive: true,
    }).catch(() => {});
  }, 0);
}
export function discardUnusedTraceView(view: TraceView) {
  if (!leases.has(view.token)) {
    const hold = { readers: 0, releaseTimer: null };
    leases.set(view.token, hold);
    scheduleRelease(view.token, hold);
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
  for (const hold of leases.values())
    if (hold.releaseTimer !== null) clearTimeout(hold.releaseTimer);
  leases.clear();
}
