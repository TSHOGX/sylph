"use client";
import { useState } from "react";
import { useT } from "@/lib/i18n";
import { traceAPI } from "@/lib/trace-api";
import { traceCacheKey } from "@/lib/trace-cache";
import { useTraceResource } from "@/lib/use-trace-resource";
import {
  representationLabel,
  type ResultRepresentation,
  type ResultRepresentationEntry,
  type TraceView,
} from "@/lib/trace-types";
import { TracePayload } from "./trace-payload";

const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
interface Context {
  reference: string;
  history: string | null;
  view: TraceView;
  maximum: number;
}

function preferred(
  entries: ResultRepresentationEntry[],
): ResultRepresentation | null {
  const available = entries.filter((entry) => entry.state.kind === "available");
  const observed = entries.filter((entry) => entry.state.kind !== "unobserved");
  const candidates = available.length ? available : observed;
  return (
    (
      candidates.find((entry) => entry.representation === "caller_result") ??
      candidates[0]
    )?.representation ?? null
  );
}

export function OperationResults({
  catalogue,
  ...context
}: Context & { catalogue: ResultRepresentationEntry[] }) {
  const t = useT();
  const [resolved, setResolved] = useState<
    Partial<Record<ResultRepresentation, ResultRepresentationEntry>>
  >({});
  const [selected, select] = useState<ResultRepresentation | null>(null);
  const [compare, setCompare] = useState(false);
  const [compared, setCompared] = useState<ResultRepresentation[]>([]);
  const entries = catalogue.map(
    (entry) => resolved[entry.representation] ?? entry,
  );
  // Choice policy has one producer and uses the full authorized catalogue. An
  // explicit user choice survives a later availability continuation.
  const selection = selected ?? preferred(entries);
  if (selection === null)
    return <p className="text-sm">{t("trace.noResults")}</p>;
  const choices = entries;
  const visible = compare ? compared : [selection];
  return (
    <section className="space-y-4">
      {!compare && (
        <label className="flex flex-col gap-1 text-sm">
          {t("trace.representation")}
          <select
            aria-label={t("trace.representation")}
            className="min-h-11 w-full min-w-0 max-w-full rounded border border-line bg-canvas px-2 text-base md:text-sm"
            value={selection}
            onChange={(event) =>
              select(event.target.value as ResultRepresentation)
            }
          >
            {choices.map((entry) => (
              <option key={entry.representation} value={entry.representation}>
                {representationLabel(entry.representation, t)}
                {entry.state.kind === "available"
                  ? ""
                  : ` · ${t(entry.state.kind === "pending" ? "trace.moreRequired" : "trace.unavailable")}`}
              </option>
            ))}
          </select>
        </label>
      )}
      {choices.length > 1 && (
        <label className="flex min-h-11 items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={compare}
            onChange={(event) => {
              setCompare(event.target.checked);
              setCompared([selection]);
            }}
          />
          {t("trace.compare")}
        </label>
      )}
      {compare && (
        <fieldset>
          <legend className="text-sm">
            {t("trace.chooseRepresentations")}
          </legend>
          {choices.map((entry) => (
            <label
              key={entry.representation}
              className="flex min-h-11 items-center gap-2 text-sm"
            >
              <input
                type="checkbox"
                checked={compared.includes(entry.representation)}
                onChange={(event) => {
                  setCompared((old) =>
                    event.target.checked
                      ? [...old, entry.representation]
                      : old.filter((value) => value !== entry.representation),
                  );
                }}
              />
              {representationLabel(entry.representation, t)}
            </label>
          ))}
        </fieldset>
      )}
      <p className="text-xs text-ink-muted">{t("trace.resourceOrdering")}</p>
      {entries
        .filter((entry) => visible.includes(entry.representation))
        .map((entry) => (
          <RepresentationResults
            key={entry.representation}
            {...context}
            entry={entry}
            onResolved={(value) =>
              setResolved((old) => ({ ...old, [value.representation]: value }))
            }
          />
        ))}
    </section>
  );
}

function RepresentationResults({
  entry,
  onResolved,
  ...context
}: Context & {
  entry: ResultRepresentationEntry;
  onResolved: (value: ResultRepresentationEntry) => void;
}) {
  const t = useT();
  return (
    <section
      className="space-y-3"
      aria-label={representationLabel(entry.representation, t)}
    >
      <h4 className="font-medium">
        {representationLabel(entry.representation, t)}
      </h4>
      {entry.state.kind === "pending" ? (
        <AvailabilityProbe
          key={`${entry.state.phase}:${entry.state.cursor}`}
          {...context}
          representation={entry.representation}
          pending={entry.state}
          onResolved={onResolved}
        />
      ) : entry.state.kind === "unobserved" ? (
        <p className="text-sm">{t("trace.noResults")}</p>
      ) : (
        <>
          {entry.state.kind === "unavailable" && (
            <p className="text-sm">{entry.state.reason}</p>
          )}
          <ResultPage {...context} representation={entry.representation} />
        </>
      )}
    </section>
  );
}

function AvailabilityProbe({
  reference,
  history,
  view,
  representation,
  pending,
  onResolved,
}: Context & {
  representation: ResultRepresentation;
  pending: Extract<ResultRepresentationEntry["state"], { kind: "pending" }>;
  onResolved: (value: ResultRepresentationEntry) => void;
}) {
  const t = useT();
  const [attempt, setAttempt] = useState(0);
  const result = useTraceResource(
    attempt
      ? traceCacheKey(view, [
          reference,
          history,
          representation,
          pending.phase,
          pending.cursor,
          "result-probe",
        ])
      : null,
    async (signal) => {
      const entry = await traceAPI.resultRepresentation(
        reference,
        view,
        history,
        representation,
        pending.phase,
        pending.cursor,
        signal,
      );
      if (!signal.aborted) onResolved(entry);
      return entry;
    },
    attempt,
    false,
  );
  return (
    <div>
      <p>{t("trace.moreRequired")}</p>
      {result.error && <p role="alert">{result.error.message}</p>}
      <button
        className={button}
        disabled={result.pending}
        onClick={() => setAttempt((old) => old + 1)}
      >
        {result.error ? t("trace.queryRetry") : t("trace.moreResults")}
      </button>
    </div>
  );
}

function ResultPage({
  reference,
  history,
  view,
  maximum,
  representation,
}: Context & { representation: ResultRepresentation }) {
  const t = useT();
  const [request, setRequest] = useState({
    cursor: null as string | null,
    direction: "after" as "after" | "before",
    attempt: 0,
  });
  const result = useTraceResource(
    traceCacheKey(view, [reference, history, representation, "result-page"]),
    (signal) =>
      traceAPI.results(
        reference,
        view,
        history,
        representation,
        request.cursor,
        request.direction,
        signal,
      ),
    request.attempt,
    false,
  );
  const go = (
    cursor: string | null | undefined,
    direction: "before" | "after",
  ) => {
    if (cursor && !result.pending)
      setRequest((old) => ({ cursor, direction, attempt: old.attempt + 1 }));
  };
  return (
    <div className="space-y-3">
      {result.pending && <p role="status">{t("trace.loadingDetails")}</p>}
      {result.error && (
        <div>
          <p role="alert">{result.error.message}</p>
          <button
            className={button}
            disabled={result.pending}
            onClick={() =>
              setRequest((old) => ({ ...old, attempt: old.attempt + 1 }))
            }
          >
            {t("trace.queryRetry")}
          </button>
        </div>
      )}
      {result.data && !result.data.items.length && (
        <p>
          {t(
            result.data.hasAfter ||
              result.data.hasBefore ||
              result.data.state !== "complete"
              ? "trace.moreRequired"
              : "trace.noResults",
          )}
        </p>
      )}
      {result.data?.items.map((occurrence) => (
        <section
          key={occurrence.reference}
          className="space-y-2 rounded border border-line p-3"
        >
          <h5 className="font-medium">
            {occurrence.label} · {occurrence.disposition.replaceAll("_", " ")}
          </h5>
          {occurrence.availability.kind !== "available" && (
            <p>{occurrence.availability.reason}</p>
          )}
          {occurrence.payloads.map((payload, index) => (
            <TracePayload
              key={index}
              access={payload}
              view={view}
              maximum={maximum}
            />
          ))}
        </section>
      ))}
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
    </div>
  );
}
