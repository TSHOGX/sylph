import type { PayloadAccess, TraceLimits, TraceView } from "./trace-types";
import type { StagedAttachment } from "../components/attachment-picker";
import {
  clearTraceLeases,
  onTraceViewReleased,
  retainTraceView,
} from "./trace-leases";
import { PayloadCache } from "./trace-payload-cache";

/** Shared bounded immutable data; a cache hit is usable only after authorization. */
export class ByteCache<T> {
  private entries = new Map<string, { value: T; bytes: number }>();
  private used = 0;
  constructor(private limit = 0) {}
  configure(limit: number) {
    this.limit = Math.max(0, limit);
    this.trim();
  }
  get bytes() {
    return this.used;
  }
  get(key: string): T | undefined {
    const entry = this.entries.get(key);
    if (!entry) return;
    this.entries.delete(key);
    this.entries.set(key, entry);
    return entry.value;
  }
  set(key: string, value: T, bytes: number) {
    const old = this.entries.get(key);
    if (old) {
      this.used -= old.bytes;
      this.entries.delete(key);
    }
    if (!Number.isSafeInteger(bytes) || bytes < 0)
      throw new Error("Invalid cache byte accounting");
    bytes += new TextEncoder().encode(key).length;
    if (!this.limit || bytes > this.limit) return;
    this.entries.set(key, { value, bytes });
    this.used += bytes;
    this.trim();
  }
  clear() {
    this.entries.clear();
    this.used = 0;
  }
  removeWhere(matches: (key: string) => boolean) {
    for (const [key, entry] of this.entries)
      if (matches(key)) {
        this.entries.delete(key);
        this.used -= entry.bytes;
      }
  }
  private trim() {
    while (this.used > this.limit) {
      const first = this.entries.keys().next().value;
      if (first === undefined) break;
      this.used -= this.entries.get(first)!.bytes;
      this.entries.delete(first);
    }
  }
}
export const traceMetadata = new ByteCache<unknown>();
export const tracePayloads = new PayloadCache();
let authorizationScope: string | null = null;
let hiddenLocationLimit = 10;
const protectedListeners = new Set<(reason: "access" | "view") => void>();
export function onTraceAccessCleared(
  listener: (reason: "access" | "view") => void,
) {
  protectedListeners.add(listener);
  return () => {
    protectedListeners.delete(listener);
  };
}
export function clearTraceAccess() {
  authorizationScope = null;
  traceMetadata.clear();
  tracePayloads.clear();
  protectedListeners.forEach((listener) => listener("access"));
}
export function invalidateTraceView() {
  traceMetadata.clear();
  tracePayloads.clear();
  protectedListeners.forEach((listener) => listener("view"));
}
export function authorizeTraceCache(scope: string, limits: TraceLimits) {
  if (authorizationScope !== null && scope !== authorizationScope)
    clearTraceAccess();
  authorizationScope = scope;
  hiddenLocationLimit = limits.maxHiddenLocations;
  while (locations.size > hiddenLocationLimit)
    locations.delete(locations.keys().next().value!);
  while (navigationViews.size > hiddenLocationLimit) {
    const first = navigationViews.keys().next().value!;
    navigationViews.get(first)!.release();
    navigationViews.delete(first);
  }
  traceMetadata.configure(limits.maxMetadataCacheBytes);
  tracePayloads.configure(limits.maxPayloadCacheBytes);
}
export function traceCacheKey(view: TraceView, key: unknown): string {
  return JSON.stringify([
    typeof location === "undefined" ? "" : location.origin,
    authorizationScope,
    view.generation,
    view.viewRevision,
    view.evidenceCut,
    view.token,
    key,
  ]);
}
export function tracePayloadKey(
  access: PayloadAccess,
  start: number,
  end?: number,
): string {
  return JSON.stringify([
    typeof location === "undefined" ? "" : location.origin,
    authorizationScope,
    access.containingAgent,
    access.selection,
    access.viaRef,
    access.payloadRef,
    access.bodyRevision,
    access.representation,
    start,
    end,
  ]);
}
export function sameTraceView(a: TraceView, b: TraceView): boolean {
  return (
    a.generation === b.generation &&
    a.evidenceCut === b.evidenceCut &&
    a.viewRevision === b.viewRevision &&
    a.token === b.token
  );
}

export interface ReaderLocation {
  anchor: string | null;
  offset: number;
  following: boolean;
  paged: boolean;
  disclosure: Set<string>;
}
const locations = new Map<string, ReaderLocation>();
const drafts = new Map<string, string>();
const draftAttachments = new Map<string, StagedAttachment[]>();
const navigationViews = new Map<
  string,
  { view: TraceView; release: () => void }
>();
export function rememberTraceNavigation(
  agent: string,
  history: string | null,
  anchor: string,
  view: TraceView,
) {
  const key = JSON.stringify([agent, history, anchor]);
  const previous = navigationViews.get(key);
  const release = retainTraceView(view);
  previous?.release();
  navigationViews.delete(key);
  navigationViews.set(key, { view, release });
  while (navigationViews.size > hiddenLocationLimit) {
    const first = navigationViews.keys().next().value!;
    navigationViews.get(first)!.release();
    navigationViews.delete(first);
  }
}
export function traceNavigationView(
  agent: string,
  history: string | null,
  anchor: string | null,
): TraceView | undefined {
  return navigationViews.get(JSON.stringify([agent, history, anchor]))?.view;
}
export function forgetTraceNavigation(
  agent: string,
  history: string | null,
  anchor: string | null,
) {
  const key = JSON.stringify([agent, history, anchor]);
  navigationViews.get(key)?.release();
  navigationViews.delete(key);
}
export function readerLocation(
  agent: string,
  history: string | null,
  maximum = hiddenLocationLimit,
): ReaderLocation {
  const key = JSON.stringify([agent, history]);
  const value = locations.get(key) ?? {
    anchor: null,
    offset: 0,
    following: true,
    paged: false,
    disclosure: new Set<string>(),
  };
  locations.delete(key);
  locations.set(key, value);
  while (locations.size > maximum)
    locations.delete(locations.keys().next().value!);
  return value;
}
export function draftFor(agent: string): string {
  return drafts.get(agent) ?? "";
}
export function saveDraft(agent: string, text: string) {
  if (text) drafts.set(agent, text);
  else drafts.delete(agent);
}
export function attachmentsFor(agent: string): StagedAttachment[] {
  return draftAttachments.get(agent) ?? [];
}
export function saveDraftAttachments(
  agent: string,
  attachments: StagedAttachment[],
) {
  if (attachments.length) draftAttachments.set(agent, attachments);
  else draftAttachments.delete(agent);
}
onTraceAccessCleared((reason) => {
  for (const entry of navigationViews.values()) entry.release();
  navigationViews.clear();
  clearTraceLeases();
  if (reason === "access") {
    locations.clear();
    drafts.clear();
    draftAttachments.clear();
  }
});

onTraceViewReleased((token) =>
  traceMetadata.removeWhere((key) => key.includes(JSON.stringify(token))),
);
