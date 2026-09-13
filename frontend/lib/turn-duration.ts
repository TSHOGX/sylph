import type { Fidelity, Turn } from "@/lib/api-types";

/** Turn row endpoints use the server clock; only stream records measure elapsed time. */
export function turnDurationMs(turn: Turn, fidelity: Fidelity): number | undefined {
  if (fidelity !== "stream") return undefined;
  const { startedAtMs: start, completedAtMs: end } = turn;
  if (start === null || end === null || !Number.isFinite(start) || !Number.isFinite(end) || end < start) {
    return undefined;
  }
  return end - start;
}
