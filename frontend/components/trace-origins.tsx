"use client";
import { useRef, useState } from "react";
import { useT } from "@/lib/i18n";
import { traceAPI } from "@/lib/trace-api";
import { traceCacheKey } from "@/lib/trace-cache";
import { useTraceResource } from "@/lib/use-trace-resource";
import { agentName, type TraceView } from "@/lib/trace-types";
import { TraceEvidence } from "./trace-evidence";
import type { InspectionRequest } from "./trace-inspector";

const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";

export interface OriginLocation {
  cursor: string | null;
  direction: "after" | "before";
  attempt: number;
}

export function OriginDirectory({
  agent,
  view,
  maximum,
  onInspect,
  request,
  setRequest,
}: {
  agent: string;
  view: TraceView;
  maximum: number;
  request: OriginLocation;
  setRequest: (value: OriginLocation) => void;
  onInspect: (request: InspectionRequest) => void;
}) {
  const t = useT();
  const heading = useRef<HTMLHeadingElement>(null);
  const [claim, setClaim] = useState<string | null>(null);
  const result = useTraceResource(
    traceCacheKey(view, [agent, "origins"]),
    (signal) =>
      traceAPI.origins(agent, view, request.cursor, request.direction, signal),
    request.attempt,
    false,
  );
  const go = (
    cursor: string | null | undefined,
    direction: "before" | "after",
  ) => {
    if (!cursor || result.pending) return;
    heading.current?.focus();
    setClaim(null);
    setRequest({ cursor, direction, attempt: request.attempt + 1 });
  };
  return (
    <section className="space-y-3" aria-label={t("trace.originAssertions")}>
      <h3 ref={heading} tabIndex={-1} className="font-medium focus-ring">
        {t("trace.originAssertions")}
      </h3>
      <p className="text-sm">{t("trace.originAssertionsNote")}</p>
      {result.pending && <p role="status">{t("trace.loadingDetails")}</p>}
      {result.error && (
        <div>
          <p role="alert">{result.error.message}</p>
          <button
            className={button}
            onClick={() =>
              setRequest({ ...request, attempt: request.attempt + 1 })
            }
          >
            {t("trace.queryRetry")}
          </button>
        </div>
      )}
      {result.data && !result.data.items.length && (
        <p>
          {t(
            result.data.state === "complete"
              ? "trace.noOrigins"
              : "trace.moreRequired",
          )}
        </p>
      )}
      <ol className="space-y-3">
        {result.data?.items.map((origin) => (
          <li
            key={origin.claim}
            className="min-w-0 rounded border border-line p-3"
          >
            {origin.origin ? (
              <button
                className={`${button} underline`}
                onClick={() =>
                  onInspect({
                    kind: "agent",
                    agent: origin.origin!.address,
                    history: null,
                    view: result.data!.view,
                  })
                }
              >
                {t("trace.origin", { agent: agentName(origin.origin, t) })}
              </button>
            ) : (
              <p className="font-medium">{t("trace.unresolvedOrigin")}</p>
            )}
            {origin.availability.kind !== "available" && (
              <p className="text-sm">{origin.availability.reason}</p>
            )}
            <button
              className={button}
              aria-expanded={claim === origin.claim}
              onClick={() =>
                setClaim(claim === origin.claim ? null : origin.claim)
              }
            >
              {t("trace.originProof")}
            </button>
            {claim === origin.claim && (
              <TraceEvidence
                key={claim}
                initialClaim={claim}
                reference={agent}
                history={null}
                view={result.data!.view}
                maximum={maximum}
                onInspect={onInspect}
              />
            )}
          </li>
        ))}
      </ol>
      <div className="flex flex-wrap">
        <button
          className={button}
          disabled={
            result.pending || Boolean(result.error) || !result.data?.hasBefore
          }
          onClick={() => go(result.data?.beforeCursor, "before")}
        >
          {t("trace.previousPage")}
        </button>
        <button
          className={button}
          disabled={
            result.pending || Boolean(result.error) || !result.data?.hasAfter
          }
          onClick={() => go(result.data?.afterCursor, "after")}
        >
          {t("trace.nextPage")}
        </button>
      </div>
    </section>
  );
}
