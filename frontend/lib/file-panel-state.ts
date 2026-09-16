"use client";
import { useSyncExternalStore } from "react";
import type { ArtifactTarget } from "./workspace-ui";
import { onTraceAccessCleared } from "./trace-cache";
interface FilePanelState {
  artifact: ArtifactTarget | null;
  artifactView: "tree" | "preview";
}
const empty: FilePanelState = { artifact: null, artifactView: "tree" };
const states = new Map<string, FilePanelState>();
const listeners = new Set<() => void>();
export function updateFilePanel(id: string, patch: Partial<FilePanelState>) {
  const next = { ...(states.get(id) ?? empty), ...patch };
  states.delete(id);
  states.set(id, next);
  while (states.size > 10) states.delete(states.keys().next().value!);
  listeners.forEach((listener) => listener());
}
export function useFilePanelState(id: string) {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () => states.get(id) ?? empty,
    () => empty,
  );
}
onTraceAccessCleared(() => {
  states.clear();
  listeners.forEach((listener) => listener());
});
