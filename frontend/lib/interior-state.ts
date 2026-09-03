import type { InteriorState } from "@/lib/api-types";

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
