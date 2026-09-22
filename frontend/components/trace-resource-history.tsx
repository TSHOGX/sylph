"use client";
import { useT } from "@/lib/i18n";
import { traceAPI } from "@/lib/trace-api";
import { traceCacheKey } from "@/lib/trace-cache";
import { useState } from "react";
import { useTraceResource } from "@/lib/use-trace-resource";
import {
  executionLabel,
  representationLabel,
  type PayloadAccess,
  type Representation,
  type ResourceObservation,
  type ResourceState,
  type TraceView,
  type TracePage,
  type ObjectContent,
} from "@/lib/trace-types";
import type { Translate } from "@/lib/i18n/locale";
import { TracePayload } from "./trace-payload";

const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
interface Context {
  reference: string;
  history: string | null;
  view: TraceView;
  cursor: string | null;
  maximum: number;
}
function useCollectionPage<T>(
  scope: string,
  first: string | null,
  read: (
    cursor: string,
    direction: "before" | "after",
    signal: AbortSignal,
  ) => Promise<TracePage<T>>,
) {
  const [request, setRequest] = useState({
    scope,
    cursor: null as string | null,
    direction: "after" as "after" | "before",
    attempt: 0,
  });
  const cursor = request.scope === scope ? request.cursor : null;
  const result = useTraceResource(
    cursor === null ? null : scope,
    (signal) => read(cursor!, request.direction, signal),
    request.attempt,
    false,
  );
  const go = (
    cursor: string | null | undefined,
    direction: "before" | "after",
  ) => {
    if (cursor && !result.pending)
      setRequest((old) => ({
        scope,
        cursor,
        direction,
        attempt: old.attempt + 1,
      }));
  };
  return {
    ...result,
    nextDisabled:
      result.pending ||
      Boolean(result.error) ||
      (result.data ? !result.data.hasAfter : !first),
    previousDisabled:
      result.pending || Boolean(result.error) || !result.data?.hasBefore,
    next: () => go(result.data?.afterCursor ?? first, "after"),
    previous: () => go(result.data?.beforeCursor, "before"),
    retry: () => go(cursor, request.direction),
  };
}
function label(state: ResourceState, t: Translate): string {
  switch (state.kind) {
    case "created":
      return t("trace.resourceCreated");
    case "running":
      return t("trace.resourceRunning");
    case "yielded":
      return t("trace.resourceYielded");
    case "closed":
      return t("trace.resourceClosed", {
        outcome: executionLabel(
          {
            reference: null,
            state: {
              kind: "terminated",
              outcome: state.outcome,
              reason: state.reason,
            },
            durations: [],
          },
          t,
        ),
      });
    case "goal_updated":
      return t("trace.resourceGoal", { status: state.status });
    case "schedule_registered":
      return t("trace.resourceRegistered");
    case "schedule_cancelled":
      return t("trace.resourceCancelled");
    case "schedule_fired":
      return t("trace.resourceFired");
    case "unknown":
      return t("trace.resourceUnknown");
  }
}
export function ResourceHistory({
  reference,
  history,
  view,
  observations,
  cursor,
  maximum,
  onInspect,
}: Context & {
  observations: ResourceObservation[];
  onInspect: (reference: string) => void;
}) {
  const t = useT();
  const pages = useCollectionPage(
    traceCacheKey(view, [reference, history, "observations"]),
    cursor,
    (next, direction, signal) =>
      traceAPI.observations(reference, view, history, next, direction, signal),
  );
  // Keep the initial preview and one explicitly requested page. Earlier pages
  // leave the DOM and their payload controls unmount; history cannot grow a cache.
  const shown = pages.data?.items ?? observations;
  return (
    <section className="space-y-3" aria-label={t("trace.resourceHistory")}>
      <h4 className="font-medium">{t("trace.resourceHistory")}</h4>
      <p className="text-xs text-muted">{t("trace.resourceOrdering")}</p>
      {!shown.length && (
        <p className="text-sm">
          {t(
            (pages.data ? pages.data.hasAfter : cursor !== null)
              ? "trace.moreRequired"
              : pages.data
                ? "trace.resourcePageEmpty"
                : "trace.noResourceStates",
          )}
        </p>
      )}
      {pages.error && <p role="alert">{pages.error.message}</p>}
      <ol className="space-y-3">
        {shown.map((observation) => (
          <li
            key={observation.claimRef}
            className="min-w-0 break-words rounded border border-line p-3"
          >
            <h5 className="text-sm font-medium">
              {label(observation.state, t)}
            </h5>
            {observation.orderBasis === "presentation_tiebreak" && (
              <p className="text-xs text-muted">
                {t("trace.resourceOrderUnknown")}
              </p>
            )}
            {(observation.state.kind === "unknown" ||
              observation.state.kind === "closed") &&
              observation.state.reason && (
                <p className="text-sm">{observation.state.reason}</p>
              )}
            {observation.source && (
              <details className="my-2 text-xs text-muted">
                <summary className="min-h-11 cursor-pointer py-3 focus-ring">
                  {t("trace.resourceSource")}
                </summary>
                <pre className="whitespace-pre-wrap break-all">
                  {JSON.stringify(observation.source, null, 2)}
                </pre>
              </details>
            )}
            {observation.state.kind === "schedule_fired" &&
              observation.state.delivery && (
                <button
                  className={button}
                  onClick={() => {
                    if (
                      observation.state.kind === "schedule_fired" &&
                      observation.state.delivery
                    )
                      onInspect(observation.state.delivery);
                  }}
                >
                  {t("trace.resourceDelivery")}
                </button>
              )}
            {observation.payloads.map((access, index) => (
              <TracePayload
                key={index}
                access={access}
                view={view}
                maximum={maximum}
              />
            ))}
          </li>
        ))}
      </ol>
      {(cursor || pages.data) && (
        <div className="flex flex-wrap gap-2">
          <button
            className={button}
            disabled={pages.previousDisabled}
            onClick={pages.previous}
          >
            {t("trace.previousResourceStates")}
          </button>
          <button
            className={button}
            disabled={pages.error ? pages.pending : pages.nextDisabled}
            onClick={pages.error ? pages.retry : pages.next}
          >
            {pages.error
              ? t("trace.queryRetry")
              : t("trace.moreResourceStates")}
          </button>
        </div>
      )}
    </section>
  );
}
export function RevisionPayloads({
  reference,
  history,
  view,
  cursor,
  maximum,
  payloads,
  representation,
  form,
  onInspect,
}: Context & {
  payloads: PayloadAccess[];
  representation: Representation;
  form: Extract<ObjectContent, { kind: "revision" }>["form"];
  onInspect: (reference: string) => void;
}) {
  const t = useT();
  const pages = useCollectionPage(
    traceCacheKey(view, [reference, history, "payloads"]),
    cursor,
    (next, direction, signal) =>
      traceAPI.revisionPayloads(
        reference,
        view,
        history,
        next,
        direction,
        signal,
      ),
  );
  return (
    <section
      className="space-y-3"
      aria-label={representationLabel(representation, t)}
    >
      <h4 className="font-medium">{representationLabel(representation, t)}</h4>
      {form.kind === "correction" && (
        <button className={button} onClick={() => onInspect(form.replaces)}>
          {t("trace.replacedRevision")}
        </button>
      )}
      {form.kind === "append" && (
        <p className="text-sm">{t("trace.appendSegment")}</p>
      )}
      {form.kind === "append" && form.predecessor && (
        <button
          className={button}
          onClick={() => {
            if (form.kind === "append" && form.predecessor)
              onInspect(form.predecessor);
          }}
        >
          {t("trace.previousRevision")}
        </button>
      )}
      {pages.error && <p role="alert">{pages.error.message}</p>}
      {(pages.data?.items ?? payloads).map((access, index) => (
        <TracePayload
          key={`${pages.data?.beforeCursor ?? "first"}:${access.bodyRevision}:${index}`}
          access={access}
          view={view}
          maximum={maximum}
        />
      ))}
      {(cursor || pages.data) && (
        <div className="flex flex-wrap gap-2">
          <button
            className={button}
            disabled={pages.previousDisabled}
            onClick={pages.previous}
          >
            {t("trace.previousPayloadParts")}
          </button>
          <button
            className={button}
            disabled={pages.error ? pages.pending : pages.nextDisabled}
            onClick={pages.error ? pages.retry : pages.next}
          >
            {pages.error ? t("trace.queryRetry") : t("trace.morePayloadParts")}
          </button>
        </div>
      )}
    </section>
  );
}
