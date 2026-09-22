import type { Translate } from "./i18n/locale";
/** The public trace read DTOs in protocol::trace::read. Addresses are opaque. */
export type AgentAddress = string;
export type TraceAddress = string;
export type Availability =
  | { kind: "available" }
  | {
      kind:
        | "pending"
        | "partial"
        | "unavailable"
        | "redacted"
        | "opaque"
        | "conflicted";
      reason: string;
    };
export interface TraceView {
  token: string;
  generation: string;
  evidenceCut: number;
  viewRevision: number;
  expiresAtMs: number;
}
export interface TraceLimits {
  maxItems: number;
  maxSummaryBytes: number;
  maxPreviewBytes: number;
  maxPayloadBytes: number;
  maxChangeItems: number;
  maxMountedRows: number;
  maxMetadataCacheBytes: number;
  maxPayloadCacheBytes: number;
  maxHiddenLocations: number;
  maxVisibleInterests: number;
}
export interface TraceCount {
  value: number | null;
  scope: string;
  complete: boolean;
}
export interface TracePage<T> {
  view: TraceView;
  items: T[];
  beforeCursor: string | null;
  afterCursor: string | null;
  hasBefore: boolean;
  hasAfter: boolean;
  total: TraceCount;
  state: "complete" | "more" | "work_budget_exhausted";
  returnedItems: number;
  returnedBytes: number;
  limits: TraceLimits;
}
export interface AgentLink {
  address: AgentAddress;
  name: string | null;
  identitySuffix: string;
  vendor: string;
}
export type ExecutionState =
  | { kind: "unavailable" }
  | {
      kind: "observed_active" | "observation_ended";
      lastObservedAtMs: number | null;
    }
  | {
      kind: "terminated";
      outcome: "succeeded" | "failed" | "cancelled" | "interrupted" | "unknown";
      reason: string | null;
    }
  | { kind: "conflicted"; reason: string };
export interface ExecutionSummary {
  reference: TraceAddress | null;
  state: ExecutionState;
  durations: {
    value: number;
    unit: "seconds" | "milliseconds" | "microseconds" | "nanoseconds";
    basis:
      | "source_wall"
      | "source_monotonic"
      | "vendor_reported"
      | "server_observed";
    clockDomain: string | null;
  }[];
}
export interface HistoryLocation {
  selection: string | null;
  label: string;
  isCurrent: boolean;
  selectionKnown: boolean;
  availability: Availability;
}
export interface TraceCoverage {
  dimension: string;
  state: Availability;
  evidence: TraceAddress | null;
}
export interface FileContext {
  sessionId: string;
  environment: string;
  workdir: string;
}
export interface AgentHeader {
  agent: AgentLink;
  environment: string;
  environmentName: string | null;
  model: string | null;
  ownExecution: ExecutionSummary;
  history: HistoryLocation;
  parent: AgentLink | null;
  origin: AgentLink | null;
  coverage: TraceCoverage[];
  descendantCount: TraceCount;
  continuingResources: TraceCount;
  requiredInput: TraceCount;
  fileContext: FileContext | null;
}
export type Author =
  | { kind: "human" | "harness" | "external"; label: string }
  | { kind: "agent"; agent: AgentLink | null; label: string }
  | { kind: "tool"; operation: TraceAddress | null; label: string }
  | { kind: "unknown" };
export type Representation =
  | "runtime"
  | "caller_result"
  | "conversation"
  | "submitted";
export interface PayloadAccess {
  payloadRef: string;
  bodyRevision: string;
  representation: Representation;
  viaRef: TraceAddress;
  containingAgent: AgentAddress;
  selection: string | null;
  mediaType: string | null;
  encoding: "binary" | "utf8" | "base64";
  totalBytes: number | null;
  availableRanges: { start: number; endExclusive: number }[];
  availability: Availability;
}
export interface PartSummary {
  partRef: string;
  kind:
    | "text"
    | "disclosed_reasoning"
    | "image"
    | "audio"
    | "video"
    | "structured"
    | "attachment"
    | "opaque";
  preview: string;
  previewTruncated: boolean;
  payload: PayloadAccess;
}
export type Issuer = { kind: "environment" | "server"; value: string };
export interface ObservationReference {
  issuer: Issuer;
  producerId: string;
  producerEpoch: string;
  producerSeq: number;
}
export interface SourcePosition {
  source: { issuer: Issuer; instance: string };
  generation: number;
  locator:
    | {
        kind: "jsonl";
        byteStart: number;
        byteEnd: number;
        recordOrdinal: number;
      }
    | { kind: "frame"; ordinal: number }
    | { kind: "snapshot"; revision: string }
    | { kind: "occurrence"; key: string };
}
export type InclusionCoverage =
  | { kind: "exact" | "summary" }
  | { kind: "parts"; parts: string[] }
  | {
      kind: "byte_ranges";
      payload: string;
      ranges: { start: number; endExclusive: number }[];
    }
  | { kind: "unknown"; reason: string };
export type OperationCategory =
  | { kind: "tool"; namespace: string | null; name: string }
  | { kind: "inference"; model: string | null }
  | { kind: "delegation" | "communication" | "wait" | "edit" }
  | {
      kind: "terminal_io";
      action: "launch" | "poll" | "write" | "resize" | "close";
    }
  | { kind: "code_cell"; language: string | null }
  | { kind: "extension"; namespace: string; name: string };
export type ResourceKind =
  | {
      kind: "terminal" | "code_cell";
      nativeHandle: string;
      incarnation: string;
    }
  | { kind: "goal" | "schedule" }
  | { kind: "artifact"; mediaType: string | null }
  | { kind: "extension"; namespace: string; name: string };
export type Metric =
  | {
      kind:
        | "input_tokens"
        | "output_tokens"
        | "cached_tokens"
        | "reasoning_tokens"
        | "total_tokens"
        | "wall_elapsed"
        | "active_duration"
        | "reported_duration";
    }
  | { kind: "cost"; pricingBasis: string }
  | { kind: "extension"; namespace: string; name: string };
export type MeasurementUnit =
  | { kind: "tokens" | "seconds" | "milliseconds" | "nanoseconds" | "bytes" | "count" }
  | { kind: "currency"; code: string }
  | { kind: "extension"; namespace: string; name: string };
export interface NativeSessionIdentity {
  environment: string;
  agent: string;
  nativeId: string;
}
export type RecordedAgentReference =
  | { kind: "session"; value: NativeSessionIdentity }
  | {
      kind: "embedded";
      value: {
        anchorSession: NativeSessionIdentity;
        namespace: string;
        nativeChildId: string;
      };
    };
export type RecordedObjectReference =
  | { kind: "agent"; ref: RecordedAgentReference }
  | {
      kind:
        | "activation"
        | "message"
        | "message_revision"
        | "delivery"
        | "operation"
        | "result"
        | "resource"
        | "history_entry"
        | "history_selection"
        | "context_checkpoint"
        | "measurement";
      ref: string;
    };
export type MeasurementMode =
  | { kind: "interval"; intervalRef: RecordedObjectReference }
  | {
      kind: "cumulative";
      counterDomain: string | null;
      resetEpoch: string | null;
      samplePosition: SourcePosition;
    }
  | { kind: "gauge" };
export type DeliveryStage =
  | { kind: "requested" | "accepted" | "queued" }
  | { kind: "admitted"; entry: string | null }
  | {
      kind: "included";
      target:
        | { kind: "inference"; operation: string }
        | { kind: "checkpoint"; checkpoint: string };
      coverage: InclusionCoverage;
    }
  | { kind: "rejected"; reason: string }
  | { kind: "cancelled"; reason: string | null };
export interface DeliverySummary {
  reference: TraceAddress;
  sender: Author;
  stages: DeliveryStage[];
  availability: Availability;
}
export type ReadingContent =
  | {
      kind: "message";
      messageRef: string;
      revisionRef: string;
      author: Author;
      purpose:
        | "task_input"
        | "progress"
        | "answer"
        | "context"
        | "notice"
        | "unclassified";
      delivery: DeliverySummary | null;
      parts: PartSummary[];
      partsTotal: number;
      partsComplete: boolean;
      partsCursor: string | null;
      originalOwner: AgentLink | null;
    }
  | {
      kind: "work";
      owner: AgentLink;
      operations: TraceCount;
      agents: TraceCount;
      assignments: TraceCount;
      failedOperations: TraceCount;
      activeResources: TraceCount;
      requiredInput: TraceCount;
      operationsAnchor: TraceAddress;
    }
  | {
      kind: "activation_boundary";
      execution: ExecutionSummary;
      triggerDeliveries: TraceAddress[];
      triggerCount: TraceCount;
    }
  | {
      kind: "context_boundary";
      checkpoint: TraceAddress;
      label: string;
      summary: PartSummary | null;
    }
  | { kind: "history_boundary"; selection: string; label: string }
  | {
      kind: "unknown";
      label: string;
      reason: string;
      payloads: PayloadAccess[];
    };
export interface ReadingUnit {
  address: string;
  anchor: TraceAddress;
  orderBasis: "native_predecessor" | "explicit_order" | "presentation_tiebreak";
  sourceSurface: string;
  activation: ExecutionSummary | null;
  content: ReadingContent;
}
export interface TraceProgress {
  evidenceCut: number;
  projectionCut: number;
  searchCut: number | null;
  pendingOutbox: number;
  oldestOutboxAgeMs: number | null;
  sourceConnected: boolean;
  fault: string | null;
}
export type ControlTarget =
  | { kind: "session"; agent: AgentAddress; sessionId: string }
  | {
      kind: "embedded";
      agent: AgentAddress;
      hostSessionId: string;
      namespace: string;
      nativeChildId: string;
      route: string;
    };
export type ControlAction =
  | "send"
  | "steer"
  | "queue"
  | "native_steer"
  | "native_queue"
  | "stop"
  | "interrupt"
  | "approval"
  | "configure";
export type ControlAvailability =
  | {
      kind: "available";
      target: ControlTarget;
      actions: ControlAction[];
      lease: string | null;
      affectedExecution: TraceAddress | null;
    }
  | { kind: "unavailable"; reason: string };
export interface ReaderBootstrap {
  view: TraceView;
  header: AgentHeader;
  window: TracePage<ReadingUnit>;
  changeCursor: string;
  progress: TraceProgress;
  control: ControlAvailability;
  authorizationScope: string;
}
export interface AgentDirectoryRow {
  agent: AgentLink;
  relation: string;
  ownExecution: ExecutionSummary;
  activations: TraceCount;
  requiredInput: TraceCount;
  coverage: TraceCoverage[];
}
export interface HistoryChoice {
  selection: string;
  label: string;
  isCurrent: boolean;
  availability: Availability;
  entryCount: TraceCount;
}
export interface ResultOccurrence {
  reference: TraceAddress;
  disposition: "progress" | "yield" | "final_return" | "refusal" | "unknown";
  representation: Representation;
  label: string;
  payloads: PayloadAccess[];
  availability: Availability;
}
export interface TraceRelation {
  target: TraceAddress;
  label: string;
  agent: AgentLink | null;
}
export type ObjectContent =
  | {
      kind: "operation";
      category: OperationCategory;
      input: PayloadAccess[];
      results: ResultOccurrence[];
      resultsCursor: string | null;
      resources: TraceRelation[];
      resourcesCursor: string | null;
    }
  | {
      kind: "resource";
      resourceKind: ResourceKind;
      state: Availability;
      payloads: PayloadAccess[];
      operationsCursor: string | null;
    }
  | {
      kind: "context";
      basis:
        | "observed_request"
        | "native_replacement"
        | "inherited_membership"
        | "summary";
      coverage: InclusionCoverage;
      memberships: TraceRelation[];
      membershipsCursor: string | null;
      payloads: PayloadAccess[];
    }
  | {
      kind: "measurement";
      metric: Metric;
      model: {
        reportedName: string;
        canonicalName: string | null;
        provider: string | null;
      } | null;
      unit: MeasurementUnit;
      value: number;
      scope: string;
      mode: MeasurementMode;
    }
  | {
      kind: "evidence";
      facts: {
        claimRef: string;
        label: string;
        summary: string;
        source: SourcePosition | null;
        payload: PayloadAccess | null;
      }[];
      nextCursor: string | null;
    }
  | { kind: "generic"; label: string; payloads: PayloadAccess[] };
export interface ObjectDetail {
  view: TraceView;
  reference: TraceAddress;
  title: string;
  owner: AgentLink | null;
  execution: ExecutionSummary | null;
  availability: Availability;
  content: ObjectContent;
  relations: TraceRelation[];
  relationsCursor: string | null;
  coverage: TraceCoverage[];
}
export interface SearchHit {
  anchor: TraceAddress;
  agent: AgentLink;
  history: HistoryLocation;
  excerpt: string;
  payload: PayloadAccess | null;
}
export type TraceChange =
  | {
      kind: "revision";
      cursor: string;
      generation: string;
      evidenceCut: number;
      viewRevision: number;
      agents: AgentAddress[];
      changed: TraceAddress[];
      removed: TraceAddress[];
      moved: TraceAddress[];
    }
  | { kind: "reset_required"; reason: string }
  | { kind: "access_changed"; authorizationScope: string };
export interface CommandRequest {
  target: ControlTarget;
  action: ControlAction;
  lease: string | null;
  affectedExecution: TraceAddress | null;
  idempotencyKey: string;
  text: string | null;
  attachments: { name: string; type: string; data: string }[];
  config: { id: string; value: unknown } | null;
  answer: { requestId: string; optionId: string; input: unknown | null } | null;
}
export interface CommandReceipt {
  editable: boolean;
  cancellable: boolean;
  commandId: string;
  observationRef: ObservationReference;
  state:
    | "accepted"
    | "queued"
    | "receiver_accepted"
    | "admitted"
    | "unconfirmed"
    | "rejected"
    | "cancelled"
    | "failed";
  revision: number;
  text: string | null;
  error: string | null;
}

export function agentName(agent: AgentLink, t?: Translate): string {
  return (
    agent.name ||
    (t
      ? t("trace.unnamed", { suffix: agent.identitySuffix })
      : `Unnamed agent · ${agent.identitySuffix}`)
  );
}
export function agentUrl(
  agent: AgentAddress,
  selection?: string | null,
  anchor?: string | null,
): string {
  const query = new URLSearchParams();
  if (selection) query.set("history", selection);
  if (anchor) query.set("anchor", anchor);
  return `/agents/${encodeURIComponent(agent)}${query.size ? `?${query}` : ""}`;
}
export function executionLabel(
  execution: ExecutionSummary,
  t?: Translate,
): string {
  const state = execution.state;
  switch (state.kind) {
    case "unavailable":
      return t ? t("trace.unknownExecution") : "Execution status unavailable";
    case "observed_active":
      return t ? t("trace.working") : "Working";
    case "observation_ended":
      return "Last observed working · outcome unknown";
    case "conflicted":
      return t
        ? t("trace.conflictedExecution")
        : "Conflicting outcome evidence";
    case "terminated":
      return t
        ? t(
            (
              {
                succeeded: "trace.completed",
                failed: "trace.failed",
                cancelled: "trace.cancelled",
                interrupted: "trace.interrupted",
                unknown: "trace.unknownOutcome",
              } as const
            )[state.outcome],
          )
        : {
            succeeded: "Completed",
            failed: "Failed",
            cancelled: "Cancelled",
            interrupted: "Interrupted",
            unknown: "Outcome unknown",
          }[state.outcome];
  }
}
export function availabilityLabel(value: Availability): string | null {
  return value.kind === "available" ? null : value.reason;
}
export const representationLabels: Record<Representation, string> = {
  runtime: "Runtime output",
  caller_result: "Returned to caller",
  conversation: "Recorded conversation result",
  submitted: "Submitted input",
};

export function representationLabel(
  value: Representation,
  t: Translate,
): string {
  return t(
    (
      {
        runtime: "trace.runtime",
        caller_result: "trace.callerResult",
        conversation: "trace.conversationResult",
        submitted: "trace.submitted",
      } as const
    )[value],
  );
}
