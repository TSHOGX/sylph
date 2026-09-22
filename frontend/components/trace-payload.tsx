"use client";
import { useT } from "@/lib/i18n";
import { useEffect, useEffectEvent, useState } from "react";
import { traceAPI } from "@/lib/trace-api";
import {
  clearTraceAccess,
  invalidateTraceView,
  onTraceAccessCleared,
  tracePayloadKey,
  tracePayloads,
} from "@/lib/trace-cache";
import { ApiError } from "@/lib/api";
import { utf8Window } from "@/lib/trace-utf8";
import { bodyWindow, nextBodyStart } from "@/lib/trace-ranges";
import {
  availabilityLabel,
  representationLabel,
  type PayloadAccess,
  type TraceView,
} from "@/lib/trace-types";
import { TraceTransfer } from "./trace-transfer";
import { Markdown } from "./markdown";

const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";

/** One explicitly opened immutable representation and one bounded byte range. */
export function TracePayload({
  access,
  view,
  maximum,
  preview = "",
  truncated = true,
}: {
  access: PayloadAccess;
  view: TraceView;
  maximum: number;
  preview?: string;
  truncated?: boolean;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [requestedStart, setStart] = useState(0);
  const [loaded, setLoaded] = useState<{
    key: string;
    bytes: Uint8Array;
    end: number;
    next: number;
    total: number | null;
    displayStart: number;
    text: string | null;
    mediaUrl: string | null;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const readable =
    access.encoding === "utf8" ||
    access.mediaType?.startsWith("text/") ||
    access.mediaType?.includes("json");
  // Markdown parsing and DOM work are synchronous. A small text window bounds that
  // work even when storage permits much larger chunks. Media can use one complete
  // retained representation within the shared byte reservation.
  const media = Boolean(
    access.mediaType && /^(image|audio|video)\//.test(access.mediaType),
  );
  const capacity =
    media &&
    access.totalBytes !== null &&
    access.totalBytes <= Math.min(maximum, 8 * 1024 * 1024)
      ? Math.max(4, access.totalBytes)
      : Math.max(4, Math.min(maximum, 64 * 1024));
  const range = bodyWindow(access, requestedStart, capacity, readable ? 3 : 0);
  const start = range?.start ?? 0;
  const sourceStart = range?.sourceStart ?? 0;
  const end = range?.end ?? 0;
  const rangeEnd = range?.rangeEnd ?? 0;
  const hasRange = range !== null;
  const key = tracePayloadKey(access, start, end);
  const current = loaded?.key === key ? loaded : null;
  const availabilityKind = access.availability.kind;
  const mediaType = access.mediaType;
  const encoding = access.encoding;
  const totalBytes = access.totalBytes;
  const read = useEffectEvent((signal: AbortSignal) =>
    traceAPI.payload(access, view, sourceStart, end, signal),
  );
  useEffect(() => {
    if (
      !open ||
      !hasRange ||
      (availabilityKind !== "available" && availabilityKind !== "partial")
    )
      return;
    if (!tracePayloads.acquire(key, end - sourceStart)) {
      queueMicrotask(() =>
        setError(
          "Close another retained body window to free the shared payload budget.",
        ),
      );
      return;
    }
    const controller = new AbortController();
    let mediaUrl: string | null = null;
    const clear = onTraceAccessCleared(() => {
      controller.abort();
      setLoaded(null);
      setOpen(false);
    });
    // An immutable cache entry still requires authorization through the current via_ref.
    // The range request checks that authorization even when its bytes are already held.
    void read(controller.signal)
      .then((result) => {
        if (controller.signal.aborted) return;
        tracePayloads.set(key, result.bytes);
        if (
          start === 0 &&
          result.end === totalBytes &&
          mediaType &&
          /^(image|audio|video)\//.test(mediaType)
        ) {
          try {
            const bytes =
              encoding === "base64"
                ? Uint8Array.from(
                    atob(new TextDecoder().decode(result.bytes)),
                    (value) => value.charCodeAt(0),
                  )
                : result.bytes;
            mediaUrl = URL.createObjectURL(
              new Blob([bytes.slice().buffer], { type: mediaType }),
            );
          } catch {
            /* The exact retained bytes remain downloadable when media decoding fails. */
          }
        }
        const retained = tracePayloads.get(key);
        if (!retained)
          throw new Error("The retained body reservation was invalidated");
        const decoded = readable
          ? utf8Window(
              retained,
              result.start,
              start,
              result.end === result.total || result.end === rangeEnd,
            )
          : null;
        setLoaded({
          key,
          bytes: retained,
          end: result.end,
          next: decoded?.next ?? result.end,
          total: result.total,
          displayStart: decoded?.start ?? result.start,
          text: decoded?.text ?? null,
          mediaUrl,
        });
        setError(null);
      })
      .catch((reason: unknown) => {
        if (!controller.signal.aborted) {
          if (
            reason instanceof ApiError &&
            (reason.status === 401 || reason.status === 403)
          )
            clearTraceAccess();
          if (reason instanceof ApiError && reason.status === 410)
            invalidateTraceView();
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      });
    return () => {
      clear();
      controller.abort();
      tracePayloads.release(key);
      if (mediaUrl) URL.revokeObjectURL(mediaUrl);
    };
  }, [
    open,
    key,
    availabilityKind,
    start,
    sourceStart,
    end,
    mediaType,
    encoding,
    totalBytes,
    readable,
    hasRange,
    rangeEnd,
  ]);
  const availability = availabilityLabel(access.availability);
  const readableRange =
    hasRange &&
    (availabilityKind === "available" || availabilityKind === "partial");
  const next =
    current && range ? nextBodyStart(range, current.next, current.total) : null;
  return (
    <div className="min-w-0 space-y-2">
      {preview && !open && <Markdown text={preview} />}
      {availability && (
        <p className="text-sm text-ink-muted">
          {access.availability.kind}: {availability}
        </p>
      )}
      {availabilityKind === "opaque" && hasRange && (
        <TraceTransfer
          access={access}
          view={view}
          text={null}
          readable={false}
        />
      )}
      {availabilityKind !== "opaque" && (
        <button
          className={button}
          disabled={!readableRange}
          aria-expanded={open}
          onClick={() => {
            if (open) setLoaded(null);
            setOpen(!open);
          }}
        >
          {open
            ? t("trace.closeContent")
            : !truncated && preview
              ? t("trace.contentActions")
              : t("trace.readRepresentation", {
                  representation: representationLabel(
                    access.representation,
                    t,
                  ).toLowerCase(),
                })}
        </button>
      )}
      {open && readableRange && (
        <div className="min-w-0 rounded border border-line p-3">
          <p className="mb-2 text-xs text-ink-muted">
            {representationLabel(access.representation, t)} ·{" "}
            {access.mediaType ?? "Media type unknown"} ·{" "}
            {t("trace.bytes", {
              start: current?.displayStart ?? start,
              end: current?.next ?? end,
            })}
            {access.totalBytes == null
              ? t("trace.totalUnknown")
              : t("trace.bytesTotal", { total: access.totalBytes })}
          </p>
          {error ? (
            <p role="alert">{error}</p>
          ) : !current ? (
            <p role="status">{t("trace.loadingContent")}</p>
          ) : readable ? (
            <Markdown text={current.text ?? ""} />
          ) : current.mediaUrl ? (
            <RetainedMedia url={current.mediaUrl} type={mediaType!} />
          ) : (
            <p>{t("trace.binaryRange", { bytes: current.bytes.byteLength })}</p>
          )}
          <TraceTransfer
            access={access}
            view={view}
            text={current?.text ?? null}
            readable={Boolean(readable)}
          />
          <div className="flex flex-wrap gap-2">
            <button
              className={button}
              disabled={range?.previous == null}
              onClick={() =>
                range?.previous != null && setStart(range.previous)
              }
            >
              {t("trace.previousRange")}
            </button>
            <button
              className={button}
              disabled={next === null}
              onClick={() => next !== null && setStart(next)}
            >
              {t("trace.nextRange")}
            </button>
            {current && (
              <button
                className={button}
                onClick={() => {
                  const blob = new Blob([current.bytes.slice().buffer], {
                    type: access.mediaType ?? "application/octet-stream",
                  });
                  const url = URL.createObjectURL(blob);
                  const link = document.createElement("a");
                  link.href = url;
                  link.download = `retained-${sourceStart}-${current.end}`;
                  link.click();
                  setTimeout(() => URL.revokeObjectURL(url), 0);
                }}
              >
                {t("trace.saveRange")}
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function RetainedMedia({ url, type }: { url: string; type: string }) {
  const t = useT();
  // The source is an authorized local Blob, so Next's image optimization endpoint
  // cannot read it. Missing source captions are stated rather than fabricated.
  if (type.startsWith("image/")) {
    return (
      // eslint-disable-next-line @next/next/no-img-element
      <img
        src={url}
        alt={t("trace.imageAlt")}
        className="max-h-[60dvh] max-w-full object-contain"
      />
    );
  }
  return (
    <div>
      {type.startsWith("audio/") ? (
        <audio
          controls
          src={url}
          aria-label={t("trace.audioAlt")}
          className="max-w-full"
        />
      ) : (
        <video
          controls
          src={url}
          aria-label={t("trace.videoAlt")}
          className="max-h-[60dvh] max-w-full"
        />
      )}
      <p className="mt-1 text-xs text-ink-muted">{t("trace.noCaptions")}</p>
    </div>
  );
}
