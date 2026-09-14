/** Delivery promises are shared by the composer and environment settings. */
export const CONTROL_DELIVERIES = [
  { id: "steer", capability: "steer", label: "control.currentTurn", hint: "control.currentTurnHint" },
  { id: "native_steer", capability: "queue_steering", label: "control.nativeSteer", hint: "control.nativeSteerHint" },
  { id: "native_queue", capability: "queue", label: "control.nativeQueue", hint: "control.nativeQueueHint" },
  { id: "queue", capability: "queue", label: "control.serverQueue", hint: "control.serverQueueHint" },
] as const;
