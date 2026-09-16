import type {
  ControlAction,
  ControlAvailability,
  ControlTarget,
} from "./trace-types";

type AvailableControl = Extract<ControlAvailability, { kind: "available" }>;
function route(target: ControlTarget): string {
  return JSON.stringify(
    target.kind === "session"
      ? [target.kind, target.agent, target.sessionId]
      : [
          target.kind,
          target.agent,
          target.hostSessionId,
          target.namespace,
          target.nativeChildId,
          target.route,
        ],
  );
}

/** Refresh authorization without silently retargeting an execution the user saw. */
export function commandControl(
  agent: string,
  action: ControlAction,
  displayed: ControlAvailability | undefined,
  fresh: ControlAvailability,
): AvailableControl {
  if (fresh.kind !== "available") throw new Error(fresh.reason);
  if (fresh.target.agent !== agent || !fresh.actions.includes(action))
    throw new Error("This action is no longer available for this agent.");
  if (["stop", "interrupt", "steer"].includes(action)) {
    if (
      displayed?.kind !== "available" ||
      route(displayed.target) !== route(fresh.target) ||
      displayed.lease !== fresh.lease ||
      displayed.affectedExecution !== fresh.affectedExecution
    ) {
      throw new Error(
        "The targeted execution changed. Refresh the controls before acting on the new execution.",
      );
    }
    return displayed;
  }
  return fresh;
}
