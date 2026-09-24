"use client";
import { useEffect, useEffectEvent, useRef, useState } from "react";
import { useT } from "@/lib/i18n";
import { formatBytes } from "@/lib/format";
import { ApiError } from "@/lib/api";
import { traceAPI } from "@/lib/trace-api";
import {
  clearTraceAccess,
  invalidateTraceView,
  onTraceAccessCleared,
  traceCacheKey,
} from "@/lib/trace-cache";
import { useTraceResource } from "@/lib/use-trace-resource";
import { utf8Window } from "@/lib/trace-utf8";
import type {
  PayloadAccess,
  StructureNode,
  StructureStatus,
  TraceView,
} from "@/lib/trace-types";

const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
interface Location {
  node: number;
  after: number | null;
}

export function TraceStructure({
  access,
  view,
  onClose,
  maximum,
}: {
  access: PayloadAccess;
  view: TraceView;
  maximum: number;
  onClose: () => void;
}) {
  const t = useT();
  const scope = traceCacheKey(view, [
    access.payloadRef,
    access.bodyRevision,
    access.representation,
    access.viaRef,
    access.selection,
    "structure",
  ]);
  const [job, setJob] = useState<StructureStatus | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const epoch = useRef(0);
  const stopJob = useRef(() => {});
  const [shown, setShown] = useState<Location | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [location, setLocation] = useState<Location>({ node: 1, after: null });
  const [trail, setTrail] = useState<Location[]>([]);
  const [scalar, setScalar] = useState<StructureNode | null>(null);
  const [attempt, setAttempt] = useState(0);
  const expiredMessage = useEffectEvent(() => t("trace.structureExpired"));
  const create = useEffectEvent(() => traceAPI.createStructure(access, view));
  useEffect(() => {
    epoch.current += 1;
    let closed = false;
    let id: string | null = null;
    let expiration: ReturnType<typeof setTimeout> | undefined;
    let expiresAt = Infinity;
    const release = () => {
      if (id) {
        const value = id;
        id = null;
        void traceAPI.closeStructure(value);
      }
    };
    const stop = () => {
      closed = true;
      epoch.current += 1;
      release();
      setJob(null);
      setScalar(null);
    };
    stopJob.current = stop;
    const expire = () => {
      if (Date.now() < expiresAt || closed) return;
      stop();
      setError(expiredMessage());
    };
    document.addEventListener("visibilitychange", expire);
    // Let admission finish so cleanup can cancel its job even when navigation
    // occurred while POST was in flight. No body bytes are returned by admission.
    void create()
      .then((value) => {
        id = value.id;
        if (closed) {
          release();
          return;
        }
        expiresAt = value.expiresAtMs;
        expiration = setTimeout(expire, Math.max(0, expiresAt - Date.now()));
        setJob(value);
      })
      .catch((reason: unknown) => {
        if (!closed)
          setError(reason instanceof Error ? reason.message : String(reason));
      });
    const clear = onTraceAccessCleared(() => {
      stop();
      setError("Access changed. Reopen the retained content.");
    });
    return () => {
      closed = true;
      epoch.current += 1;
      clear();
      if (expiration) clearTimeout(expiration);
      document.removeEventListener("visibilitychange", expire);
      release();
    };
  }, [scope]);
  const id = job?.id;
  useEffect(() => {
    if (!id) return;
    const currentEpoch = epoch.current;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      if (document.hidden) {
        timer = setTimeout(poll, 750);
        return;
      }
      try {
        const value = await traceAPI.structure(
          id,
          location.node,
          location.after,
          controller.signal,
        );
        if (controller.signal.aborted || currentEpoch !== epoch.current) return;
        setJob(value);
        setShown(location);
        setError(null);
        if (value.state === "building") timer = setTimeout(poll, 750);
      } catch (reason) {
        if (!controller.signal.aborted && currentEpoch === epoch.current) {
          if (reason instanceof ApiError && reason.status === 404)
            stopJob.current();
          if (
            reason instanceof ApiError &&
            (reason.status === 401 || reason.status === 403)
          )
            clearTraceAccess();
          if (reason instanceof ApiError && reason.status === 410)
            invalidateTraceView();
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      }
    };
    void poll();
    return () => {
      controller.abort();
      if (timer) clearTimeout(timer);
    };
  }, [id, location, attempt]);
  const go = (next: Location) => {
    heading.current?.focus();
    setTrail((old) => [...old, location].slice(-32));
    setLocation(next);
    setScalar(null);
    setError(null);
  };
  const current =
    shown?.node === location.node && shown.after === location.after;
  return (
    <section
      className="min-w-0 space-y-3 rounded border border-line p-3"
      aria-label={t("trace.structureTitle")}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 ref={heading} tabIndex={-1} className="font-medium focus-ring">
          {t("trace.structureTitle")}
        </h4>
        <button className={button} onClick={onClose}>
          {t("trace.structureClose")}
        </button>
      </div>
      {job && (
        <p className="text-xs text-ink-muted">
          {t("trace.structureLimits", {
            bytes: formatBytes(job.limits.maxBytes),
            nodes: job.limits.maxNodes.toLocaleString(),
            depth: job.limits.maxDepth,
          })}
        </p>
      )}
      {error && (
        <div>
          <p role="alert">{error}</p>
          {id && (
            <button
              className={button}
              onClick={() => setAttempt((value) => value + 1)}
            >
              {t("trace.queryRetry")}
            </button>
          )}
        </div>
      )}
      {(!job || job.state === "building") && !error && (
        <p role="status">
          {job
            ? t("trace.structureBuilding", {
                bytes: job.bytesRead,
                total: job.totalBytes,
                nodes: job.nodes,
              })
            : t("trace.loadingDetails")}
        </p>
      )}
      {job?.state === "failed" && <p role="alert">{job.error}</p>}
      {job?.state === "ready" && (
        <>
          <div className="flex flex-wrap">
            <button
              className={button}
              disabled={!trail.length}
              onClick={() => {
                const prior = trail.at(-1);
                if (prior) {
                  heading.current?.focus();
                  setLocation(prior);
                  setTrail((old) => old.slice(0, -1));
                  setScalar(null);
                }
              }}
            >
              {t("trace.structureBack")}
            </button>
            <button
              className={button}
              disabled={location.node === 1 && location.after === null}
              onClick={() => go({ node: 1, after: null })}
            >
              {t("trace.structureRoot")}
            </button>
          </div>
          {!current ? (
            !error && <p role="status">{t("trace.loadingDetails")}</p>
          ) : (
            <>
              <p className="break-all">
                {job.node!.parent === null
                  ? t("trace.structureRoot")
                  : (job.node!.name?.text ?? `[${job.node!.ordinal}]`)}{" "}
                · {job.node!.kind} ·{" "}
                {t("trace.structureCount", {
                  count: job.node!.children.toLocaleString(),
                })}
              </p>
              {job.node!.children === 0 &&
                (job.node!.kind === "object" || job.node!.kind === "array") && (
                  <p>{t("trace.structureEmpty")}</p>
                )}
              {job.node!.preview && (
                <pre className="overflow-x-auto whitespace-pre-wrap break-all text-sm">
                  {job.node!.preview.text}
                </pre>
              )}
              {job.node!.preview?.truncated && (
                <button className={button} onClick={() => setScalar(job.node!)}>
                  {t("trace.structureValue")}
                </button>
              )}
              <ol className="space-y-2">
                {job.items?.map((node) => (
                  <li
                    key={node.id}
                    className="min-w-0 rounded border border-line p-2"
                  >
                    <p className="break-all font-mono text-sm">
                      {node.name?.text ?? `[${node.ordinal}]`}
                      {node.name?.truncated ? "…" : ""}
                    </p>
                    <p className="text-xs text-ink-muted">
                      {node.kind} · {node.start}–{node.end}
                    </p>
                    {node.kind === "object" || node.kind === "array" ? (
                      <button
                        className={button}
                        onClick={() => go({ node: node.id, after: null })}
                      >
                        {t("trace.structureOpen", {
                          kind: node.kind,
                          count: node.children.toLocaleString(),
                        })}
                      </button>
                    ) : (
                      <>
                        <pre className="overflow-x-auto whitespace-pre-wrap break-all text-sm">
                          {node.preview?.text}
                        </pre>
                        {node.preview?.truncated && (
                          <p className="text-xs">
                            {t("trace.structureTruncated")}
                          </p>
                        )}
                        <button
                          className={button}
                          onClick={() => setScalar(node)}
                        >
                          {t("trace.structureValue")}
                        </button>
                      </>
                    )}
                  </li>
                ))}
              </ol>
              <button
                className={button}
                disabled={job.after == null}
                onClick={() => go({ node: location.node, after: job.after! })}
              >
                {t("trace.nextPage")}
              </button>
            </>
          )}
        </>
      )}
      {scalar && job && (
        <ScalarRange
          key={scalar.id}
          access={access}
          view={job.view}
          maximum={maximum}
          node={scalar}
        />
      )}
    </section>
  );
}

function ScalarRange({
  access,
  view,
  node,
  maximum,
}: {
  access: PayloadAccess;
  view: TraceView;
  node: StructureNode;
  maximum: number;
}) {
  const t = useT();
  const [start, setStart] = useState(node.start);
  const end = Math.min(node.end, start + Math.min(maximum, 16 * 1024));
  const result = useTraceResource(
    traceCacheKey(view, [node.id, start, "structure-value"]),
    async (signal) => {
      const value = await traceAPI.payload(access, view, start, end, signal);
      return utf8Window(value.bytes, start, start, value.end === node.end);
    },
    0,
    false,
  );
  return (
    <div className="min-w-0 space-y-2 rounded border border-line p-2">
      <h5>
        {t("trace.structureValue")} · {start}–{result.data?.next ?? end}
      </h5>
      {result.error && <p role="alert">{result.error.message}</p>}
      {result.data ? (
        <pre className="overflow-x-auto whitespace-pre-wrap break-all text-sm">
          {result.data.text}
        </pre>
      ) : (
        <p role="status">{t("trace.loadingContent")}</p>
      )}
      <button
        className={button}
        disabled={start === node.start}
        onClick={() => setStart(node.start)}
      >
        {t("trace.firstPage")}
      </button>
      <button
        className={button}
        disabled={!result.data || result.data.next >= node.end}
        onClick={() => {
          if (result.data) setStart(result.data.next);
        }}
      >
        {t("trace.nextRange")}
      </button>
    </div>
  );
}
