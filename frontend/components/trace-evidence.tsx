"use client";
import { useState } from "react";
import { useT } from "@/lib/i18n";
import { traceAPI } from "@/lib/trace-api";
import { traceCacheKey } from "@/lib/trace-cache";
import { useTraceResource } from "@/lib/use-trace-resource";
import type { ClaimDetail, TraceView } from "@/lib/trace-types";
import type { InspectionRequest } from "./trace-inspector";
const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
const code =
  "max-h-80 overflow-auto whitespace-pre-wrap break-all rounded bg-sunken p-3 text-xs";
type Props = {
  initialClaim?: string;
  maximum: number;
  reference: string;
  history: string | null;
  view: TraceView;
  onInspect: (value: InspectionRequest) => void;
};

/** Explicit inspection has its own one-page state and never hydrates source bodies automatically. */
export function TraceEvidence(props: Props) {
  const t = useT();
  const [open, setOpen] = useState(Boolean(props.initialClaim));
  return (
    <section className="min-w-0">
      <button
        className={button}
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {t("trace.inspectEvidence")}
      </button>
      {open && <ClaimPage {...props} />}
    </section>
  );
}
function ClaimPage(props: Props) {
  const t = useT();
  const [cursor, setCursor] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [selected, setSelected] = useState<string | null>(
    props.initialClaim ?? null,
  );
  const [corrects, setCorrects] = useState<string | null>(null);
  const result = useTraceResource(
    traceCacheKey(props.view, [
      "evidence",
      props.reference,
      props.history,
      cursor,
      corrects,
    ]),
    (signal) =>
      traceAPI.evidence(
        props.reference,
        props.view,
        props.history,
        cursor,
        signal,
        corrects,
      ),
    attempt,
    false,
  );
  return (
    <div className="space-y-3">
      <h4 className="font-medium">
        {corrects ? t("trace.laterCorrections") : t("trace.claims")}
      </h4>
      <p className="text-sm">{t("trace.retainedClaimsNote")}</p>
      {corrects && (
        <button
          className={button}
          onClick={() => {
            setCorrects(null);
            setCursor(null);
            setSelected(null);
          }}
        >
          {t("trace.claims")}
        </button>
      )}
      {result.error && <p role="alert">{result.error.message}</p>}
      {result.pending && <p role="status">{t("trace.loadingDetails")}</p>}
      {result.data && (
        <>
          {!result.data.items.length && <p>{t("trace.noClaims")}</p>}
          <ul className="space-y-2">
            {result.data.items.map((claim) => (
              <li key={claim.claimRef}>
                <p className="break-all text-sm">
                  {claim.factKind} · {claim.ruleId} · {claim.mapperBuild}
                </p>
                <pre className={code}>
                  {claim.preview}
                  {claim.previewTruncated ? "…" : ""}
                </pre>
                <button
                  className={button}
                  onClick={() => setSelected(claim.claimRef)}
                >
                  {t("trace.readClaim")}
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      <button
        className={button}
        disabled={result.pending || (!result.error && !result.data?.hasAfter)}
        onClick={() => {
          setSelected(null);
          if (result.error) setAttempt(attempt + 1);
          else setCursor(result.data?.afterCursor ?? null);
        }}
      >
        {result.error ? t("trace.queryRetry") : t("trace.moreSearch")}
      </button>
      {selected && (
        <ClaimPanel
          key={selected}
          {...props}
          claim={selected}
          onClaim={setSelected}
          onCorrections={(claim) => {
            setCorrects(claim);
            setCursor(null);
            setSelected(null);
          }}
        />
      )}
    </div>
  );
}
function ClaimPanel(
  props: Props & {
    claim: string;
    onClaim: (claim: string) => void;
    onCorrections: (claim: string) => void;
  },
) {
  const t = useT();
  const [attempt, setAttempt] = useState(0);
  const result = useTraceResource(
    traceCacheKey(props.view, [
      "claim",
      props.reference,
      props.claim,
      props.history,
    ]),
    (signal) =>
      traceAPI.claim(
        props.reference,
        props.claim,
        props.view,
        props.history,
        signal,
      ),
    attempt,
    false,
  );
  if (result.error)
    return (
      <div>
        <p role="alert">{result.error.message}</p>
        <button className={button} onClick={() => setAttempt(attempt + 1)}>
          {t("trace.queryRetry")}
        </button>
      </div>
    );
  if (!result.data) return <p role="status">{t("trace.loadingDetails")}</p>;
  return <ClaimContent {...props} detail={result.data} />;
}
function ClaimContent(
  props: Props & {
    claim: string;
    onClaim: (claim: string) => void;
    onCorrections: (claim: string) => void;
    detail: ClaimDetail;
  },
) {
  const t = useT();
  const [source, setSource] = useState<string | null>(null);
  const { detail } = props;
  // Facts are bounded explicit metadata, rendered as text. Never interpret them as HTML.
  return (
    <section className="space-y-3 border-t border-line pt-3">
      <h4 className="font-medium">{t("trace.evidenceFact")}</h4>
      <p className="break-all text-xs">
        {detail.claim.ruleId} · {detail.claim.mapperBuild}
      </p>
      <p className="break-all text-xs">{detail.claim.claimRef}</p>
      {detail.claim.sourceRevision && (
        <p className="break-all text-xs">{detail.claim.sourceRevision}</p>
      )}
      <FactText value={detail.claim.fact} maximum={props.maximum} />
      <button
        className={button}
        onClick={() => props.onCorrections(props.claim)}
      >
        {t("trace.laterCorrections")}
      </button>
      <ReferenceButtons
        values={detail.claim.supersedes}
        label={t("trace.claimCorrects")}
        onSelect={props.onClaim}
      />
      <ReferenceButtons
        values={detail.related}
        label={t("trace.related")}
        onSelect={(reference) => props.onInspect({ kind: "object", reference })}
      />
      <ReferenceButtons
        values={detail.sources}
        title={t("trace.evidenceSources")}
        label={t("trace.readSource")}
        onSelect={setSource}
      />
      {source && <SourceRange key={source} {...props} observation={source} />}
    </section>
  );
}
function SourceRange(props: Props & { claim: string; observation: string }) {
  const t = useT();
  const [start, setStart] = useState(0);
  const [attempt, setAttempt] = useState(0);
  const result = useTraceResource(
    traceCacheKey(props.view, [
      "evidence-source",
      props.reference,
      props.claim,
      props.observation,
      props.history,
      start,
    ]),
    (signal) =>
      traceAPI.evidenceSource(
        props.reference,
        props.claim,
        props.observation,
        props.view,
        props.history,
        start,
        signal,
      ),
    attempt,
    false,
  );
  const source = result.data;
  let text = "";
  let binary = false;
  if (source) {
    const bytes = Uint8Array.from(atob(source.bytesBase64), (value) =>
      value.charCodeAt(0),
    );
    try {
      text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
        bytes,
      );
    } catch {
      binary = true;
      text = Array.from(bytes, (byte) =>
        byte.toString(16).padStart(2, "0"),
      ).join(" ");
    }
  }
  return (
    <div className="space-y-2">
      {result.error && <p role="alert">{result.error.message}</p>}
      {result.pending && <p role="status">{t("trace.loadingContent")}</p>}
      {source && (
        <>
          <h5 className="font-medium">{t("trace.captureMetadata")}</h5>
          <FactText value={source.capture} maximum={props.maximum} />
          <p className="text-xs">
            {source.start}–{source.endExclusive} / {source.totalBytes ?? "?"}
          </p>
          {binary && <p>{t("trace.sourceHex")}</p>}
          <pre data-evidence-bytes className={code}>
            {text}
          </pre>
        </>
      )}
      <button
        className={button}
        disabled={result.pending || start === 0}
        onClick={() =>
          setStart(Math.max(0, start - (source?.maxRangeBytes ?? start)))
        }
      >
        {t("trace.previousRange")}
      </button>
      <button
        className={button}
        disabled={
          result.pending ||
          (!result.error &&
            (!source ||
              source.totalBytes === null ||
              source.endExclusive >= source.totalBytes))
        }
        onClick={() => {
          if (result.error) setAttempt(attempt + 1);
          else if (source) setStart(source.endExclusive);
        }}
      >
        {result.error ? t("trace.queryRetry") : t("trace.nextRange")}
      </button>
    </div>
  );
}

function ReferenceButtons({
  values,
  title,
  label,
  onSelect,
}: {
  values: string[];
  title?: string;
  label: string;
  onSelect: (value: string) => void;
}) {
  const t = useT();
  const [offset, setOffset] = useState(0);
  if (!values.length) return null;
  return (
    <div className="space-y-1">
      <h5 className="font-medium">{title ?? label}</h5>
      <ul>
        {values.slice(offset, offset + 8).map((value, index) => (
          <li key={value}>
            <button className={button} onClick={() => onSelect(value)}>
              {label} {offset + index + 1}
            </button>
          </li>
        ))}
      </ul>
      {values.length > 8 && (
        <div>
          <button
            className={button}
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - 8))}
          >
            {t("trace.previousRange")}
          </button>
          <button
            className={button}
            disabled={offset + 8 >= values.length}
            onClick={() => setOffset(offset + 8)}
          >
            {t("trace.moreSearch")}
          </button>
        </div>
      )}
    </div>
  );
}
function FactText({ value, maximum }: { value: unknown; maximum: number }) {
  const t = useT();
  const [start, setStart] = useState(0);
  const text = JSON.stringify(value, null, 2);
  const limit = Math.max(4, Math.min(maximum, 16 * 1024));
  let end = Math.min(text.length, start + limit);
  if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
  return (
    <div>
      <pre data-evidence-fact className={code}>
        {text.slice(start, end)}
      </pre>
      {text.length > limit && (
        <div>
          <button
            className={button}
            disabled={start === 0}
            onClick={() => setStart(0)}
          >
            {t("trace.firstFactRange")}
          </button>
          <button
            className={button}
            disabled={end >= text.length}
            onClick={() => setStart(end)}
          >
            {t("trace.nextFactRange")}
          </button>
        </div>
      )}
    </div>
  );
}
