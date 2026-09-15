"use client";
import { useSyncExternalStore } from "react";
import type { ArtifactTarget } from "./workspace-ui";
import type { ExecutionNode } from "./execution-store";

export interface TrajectoryTarget {
  sessionId: string;
  nodeId?: string | null;
  name?: string | null;
}
export interface DetailTarget {
  sessionId: string;
  node: ExecutionNode;
}
export interface PanelState {
  artifact: ArtifactTarget | null;
  artifactView: "tree" | "preview";
  tabs: string[];
  active: string;
  maximized: boolean;
  width: number | null;
  trail: TrajectoryTarget[];
  directory: boolean;
  search: string;
  directoryScroll: number;
  detail: DetailTarget | null;
  scrolls: Record<string, number>;
}
const empty: PanelState = {
  artifact: null,
  artifactView: "tree",
  tabs: ["files"],
  active: "files",
  maximized: false,
  width: null,
  trail: [],
  directory: true,
  search: "",
  directoryScroll: 0,
  detail: null,
  scrolls: {},
};
const states = new Map<string, PanelState>();
const listeners = new Set<() => void>();
export function panelSnapshot(id: string) {
  return states.get(id) ?? empty;
}
export function updatePanel(id: string, patch: Partial<PanelState>) {
  const next = { ...panelSnapshot(id), ...patch };
  states.delete(id);
  states.set(id, next);
  while (states.size > 10) states.delete(states.keys().next().value!);
  listeners.forEach((fn) => fn());
}
export function activatePanel(id: string, tab: string) {
  const state = panelSnapshot(id);
  updatePanel(id, {
    active: tab,
    tabs: state.tabs.includes(tab) ? state.tabs : [...state.tabs, tab],
  });
}
export function usePanelState(id: string) {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => {
        listeners.delete(fn);
      };
    },
    () => panelSnapshot(id),
    () => empty,
  );
}
