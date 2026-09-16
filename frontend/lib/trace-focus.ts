"use client";
import { useSyncExternalStore } from "react";
import type { FileContext } from "./trace-types";
import { onTraceAccessCleared } from "./trace-cache";

interface Focus {
  agent: string;
  context: FileContext | null;
}
let focus: Focus | null = null;
const listeners = new Set<() => void>();
function emit() {
  listeners.forEach((listener) => listener());
}
export function setTraceFocus(
  agent: string,
  context: FileContext | null,
): () => void {
  const next = { agent, context };
  focus = next;
  emit();
  return () => {
    if (focus === next) {
      focus = null;
      emit();
    }
  };
}
export function useTraceFocus(): Focus | null {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () => focus,
    () => null,
  );
}
onTraceAccessCleared(() => {
  focus = null;
  emit();
});
