import type { InteriorState } from "@/lib/api-types";

export function interiorMessage(state: InteriorState | undefined): string {
  if (!state) return "No interior observation was recorded for this delegation.";
  const messages: Record<InteriorState, string> = {
    pending: "Waiting for this delegation's interior to become available.",
    partial: "Only part of this delegation's interior was recorded.",
    full: "This delegation's complete record contains no interior events.",
    forwards_nothing: "This delegation forwarded none of its own events.",
    unreadable: "This delegation's interior source could not be read.",
  };
  return messages[state];
}

/**
 * Merge producer-authored observations for one delegated trajectory.
 *
 * This ordering is evidence strength, not lifecycle order: a complete readable source
 * must survive a later `pending` notification when stdout and native readers interleave.
 */
export function mergeInteriorState(
  current: InteriorState | undefined,
  incoming: InteriorState,
): InteriorState {
  const strength: Record<InteriorState, number> = {
    pending: 1,
    forwards_nothing: 2,
    unreadable: 3,
    partial: 4,
    full: 5,
  };
  return !current || strength[incoming] >= strength[current] ? incoming : current;
}
