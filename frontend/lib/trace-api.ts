import { ApiError, fetchAPI } from "./api";
import type {
  AgentDirectoryRow,
  ClaimSummary,
  ClaimDetail,
  EvidenceBytes,
  CommandReceipt,
  CommandRequest,
  ControlAvailability,
  ExecutionSummary,
  HistoryChoice,
  ObjectDetail,
  PartSummary,
  PayloadAccess,
  ReaderBootstrap,
  ReadingUnit,
  ResultOccurrence,
  ResultRepresentationEntry,
  ResultRepresentation,
  ResourceObservation,
  SearchHit,
  TracePage,
  TraceRelation,
  TraceView,
} from "./trace-types";

const base = "/trace";
const segment = encodeURIComponent;
function query(values: Record<string, string | number | null | undefined>) {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(values))
    if (value != null) query.set(key, String(value));
  return query.size ? `?${query}` : "";
}
export const traceAPI = {
  bootstrap: (
    agent: string,
    history?: string | null,
    anchor?: string | null,
    signal?: AbortSignal,
    view?: TraceView | null,
  ) =>
    fetchAPI<ReaderBootstrap>(
      `${base}/agents/${segment(agent)}${query({ selection: history, around: anchor, at_snapshot: view?.token })}`,
      { signal },
    ),
  history: (
    agent: string,
    view: TraceView,
    history: string | null,
    direction: "before" | "after" | "around",
    cursor: string,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<ReadingUnit>>(
      `${base}/agents/${segment(agent)}/history${query({ at_snapshot: view.token, selection: history, [direction]: cursor })}`,
      { signal },
    ),
  agents: (
    agent: string,
    view: TraceView,
    search: string,
    filter: string,
    cursor?: string | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<AgentDirectoryRow>>(
      `${base}/agents/${segment(agent)}/relations${query({ at_snapshot: view.token, relation: "spawn", direction: "outgoing", q: search, filter, after: cursor })}`,
      { signal },
    ),
  directoryCount: (
    agent: string,
    view: TraceView,
    search: string,
    filter: string,
    cursor: string | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<AgentDirectoryRow>>(
      `${base}/agents/${segment(agent)}/relations${query({ at_snapshot: view.token, relation: "spawn", direction: "outgoing", q: search, filter, after: cursor, count_only: "true" })}`,
      { signal },
    ),
  histories: (
    agent: string,
    view: TraceView,
    cursor?: string | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<HistoryChoice>>(
      `${base}/agents/${segment(agent)}/histories${query({ at_snapshot: view.token, after: cursor })}`,
      { signal },
    ),
  activations: (
    agent: string,
    view: TraceView,
    position: { direction: "before" | "after"; cursor: string } | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<ExecutionSummary>>(
      `${base}/agents/${segment(agent)}/activations${query({ at_snapshot: view.token, ...(position ? { [position.direction]: position.cursor } : {}) })}`,
      { signal },
    ),
  object: (
    reference: string,
    view: TraceView,
    history: string | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<ObjectDetail>(
      `${base}/objects/${segment(reference)}${query({ at_snapshot: view.token, selection: history })}`,
      { signal },
    ),
  evidence: (
    reference: string,
    view: TraceView,
    history: string | null,
    cursor: string | null,
    signal?: AbortSignal,
    corrects?: string | null,
  ) =>
    fetchAPI<TracePage<ClaimSummary>>(
      `${base}/objects/${segment(reference)}/evidence${query({ at_snapshot: view.token, selection: history, after: cursor, limit: 8, corrects })}`,
      { signal, cache: "no-store" },
    ),
  claim: (
    reference: string,
    claim: string,
    view: TraceView,
    history: string | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<ClaimDetail>(
      `${base}/objects/${segment(reference)}/claims/${segment(claim)}${query({ at_snapshot: view.token, selection: history })}`,
      { signal, cache: "no-store" },
    ),
  evidenceSource: (
    reference: string,
    claim: string,
    observation: string,
    view: TraceView,
    history: string | null,
    start: number,
    signal?: AbortSignal,
  ) =>
    fetchAPI<EvidenceBytes>(
      `${base}/objects/${segment(reference)}/claims/${segment(claim)}/sources/${segment(observation)}${query({ at_snapshot: view.token, selection: history, start })}`,
      { signal, cache: "no-store" },
    ),
  parts: (
    reference: string,
    view: TraceView,
    cursor: string,
    via: string,
    selection: string | null,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<PartSummary>>(
      `${base}/objects/${segment(reference)}/parts${query({ at_snapshot: view.token, after: cursor, via_ref: via, selection })}`,
      { signal },
    ),
  search: (
    agent: string,
    selection: string | null,
    q: string,
    cursor?: string | null,
    signal?: AbortSignal,
    view?: TraceView | null,
  ) =>
    fetchAPI<TracePage<SearchHit>>(
      `${base}/search${query({ agent, selection, at_snapshot: cursor ? view?.token : null, q, cursor })}`,
      { signal },
    ),
  globalSearch: (
    q: string,
    cursor?: string | null,
    signal?: AbortSignal,
    view?: TraceView | null,
  ) =>
    fetchAPI<TracePage<SearchHit>>(
      `${base}/search${query({ q, cursor, at_snapshot: cursor ? view?.token : null })}`,
      { signal },
    ),
  results: (
    reference: string,
    view: TraceView,
    history: string | null,
    representation: ResultRepresentation,
    cursor: string | null,
    direction: "before" | "after",
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<ResultOccurrence>>(
      `${base}/objects/${segment(reference)}/relations${query({ at_snapshot: view.token, selection: history, kind: "results", representation, limit: 16, [direction]: cursor })}`,
      { signal },
    ),
  resultRepresentation: (
    reference: string,
    view: TraceView,
    history: string | null,
    representation: ResultRepresentation,
    phase: "payload" | "occurrence",
    cursor: string,
    signal?: AbortSignal,
  ) =>
    fetchAPI<ResultRepresentationEntry>(
      `${base}/objects/${segment(reference)}/relations${query({ at_snapshot: view.token, selection: history, kind: "result_representations", representation, result_phase: phase, after: cursor })}`,
      { signal },
    ),
  observations: (
    reference: string,
    view: TraceView,
    history: string | null,
    cursor: string,
    direction: "after" | "before",
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<ResourceObservation>>(
      `${base}/objects/${segment(reference)}/relations${query({ at_snapshot: view.token, selection: history, kind: "observations", limit: 16, [direction]: cursor })}`,
      { signal },
    ),
  revisionPayloads: (
    reference: string,
    view: TraceView,
    history: string | null,
    cursor: string,
    direction: "after" | "before",
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<PayloadAccess>>(
      `${base}/objects/${segment(reference)}/relations${query({ at_snapshot: view.token, selection: history, kind: "payloads", limit: 16, [direction]: cursor })}`,
      { signal },
    ),
  relations: (
    reference: string,
    view: TraceView,
    history: string | null,
    kind: "resources" | "memberships" | "relations",
    cursor: string,
    signal?: AbortSignal,
  ) =>
    fetchAPI<TracePage<TraceRelation>>(
      `${base}/objects/${segment(reference)}/relations${query({ at_snapshot: view.token, selection: history, kind, after: cursor })}`,
      { signal },
    ),
  control: (agent: string, signal?: AbortSignal) =>
    fetchAPI<ControlAvailability>(`${base}/agents/${segment(agent)}/control`, {
      signal,
    }),
  command: (body: CommandRequest) =>
    fetchAPI<CommandReceipt>(`${base}/commands`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  editCommand: (id: string, revision: number, text: string) =>
    fetchAPI<CommandReceipt>(`${base}/commands/${segment(id)}`, {
      method: "PATCH",
      body: JSON.stringify({ revision, text }),
    }),
  cancelCommand: (id: string, revision: number) =>
    fetchAPI<CommandReceipt>(
      `${base}/commands/${segment(id)}${query({ revision })}`,
      { method: "DELETE" },
    ),
  commands: (agent: string, signal?: AbortSignal) =>
    fetchAPI<{ commands: CommandReceipt[] }>(
      `${base}/agents/${segment(agent)}/commands`,
      { signal },
    ),
  async transfer(
    access: PayloadAccess,
    view: TraceView,
    kind: "copy" | "download",
    buffered: boolean,
    signal: AbortSignal,
  ): Promise<Response> {
    const url = `/api/v1${base}/payloads/${segment(access.payloadRef)}/transfer${query({ at_snapshot: view.token, via_ref: access.viaRef, selection: access.selection, revision: access.bodyRevision, representation: access.representation, kind, buffered: String(buffered) })}`;
    const response = await fetch(url, {
      signal,
      credentials: "include",
      cache: "no-store",
    });
    if (!response.ok) {
      const body = await response.json();
      throw new ApiError(
        response.status,
        body.error?.code ?? "transfer_failed",
        body.error?.message ?? "Observed-content transfer failed",
      );
    }
    return response;
  },
  async payload(
    access: PayloadAccess,
    view: TraceView,
    start: number,
    end: number,
    signal: AbortSignal,
  ): Promise<{
    bytes: Uint8Array;
    start: number;
    end: number;
    total: number | null;
  }> {
    const url = `/api/v1${base}/payloads/${segment(access.payloadRef)}${query({ at_snapshot: view.token, via_ref: access.viaRef, selection: access.selection, revision: access.bodyRevision, representation: access.representation, start, end })}`;
    const response = await fetch(url, {
      signal,
      credentials: "include",
      cache: "no-store",
    });
    if (!response.ok)
      throw new ApiError(
        response.status,
        `TRACE_PAYLOAD_${response.status}`,
        response.status === 410
          ? "This view expired. Refresh around the current item."
          : `Could not read retained content (${response.status})`,
      );
    const reader = response.body?.getReader();
    if (!reader) throw new Error("Payload response has no body");
    const chunks: Uint8Array[] = [];
    let length = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        length += value.length;
        if (length > end - start)
          throw new Error("Payload exceeded the requested byte bound");
        chunks.push(value);
      }
    } finally {
      await reader.cancel().catch(() => {});
      reader.releaseLock();
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    const range = response.headers
      .get("content-range")
      ?.match(/^bytes (\d+)-(\d+)\/(\d+|\*)$/);
    if (
      range &&
      (Number(range[1]) !== start ||
        Number(range[2]) + 1 > end ||
        Number(range[2]) + 1 - start !== length)
    )
      throw new Error(
        "Payload response range does not match its authorized request",
      );
    return {
      bytes,
      start: range ? Number(range[1]) : start,
      end: range ? Number(range[2]) + 1 : start + length,
      total: range && range[3] !== "*" ? Number(range[3]) : access.totalBytes,
    };
  },
};
