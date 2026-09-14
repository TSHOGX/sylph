import type { StreamEvent } from "@/lib/api-types";

/** Durable terminal states, shared by live notices and history handoff. */
export const TERMINAL_TURN_STATUSES: ReadonlySet<string> = new Set([
  "completed", "failed", "cancelled", "interrupted",
]);

/** A closed socket or a vendor turn event cannot substitute for durable settlement. */
export function terminalStateOf(event: StreamEvent): string | null {
  if (event.kind !== "status") return null;
  const state = (event.data as { state?: unknown } | undefined)?.state;
  return typeof state === "string" && TERMINAL_TURN_STATUSES.has(state) ? state : null;
}
