"use client";
import { useEffect, useRef, useState } from "react";
import { useT } from "@/lib/i18n";
import { ApiError } from "@/lib/api";
import { traceAPI } from "@/lib/trace-api";
import {
  onTraceAccessCleared,
  clearTraceAccess,
  invalidateTraceView,
} from "@/lib/trace-cache";
import {
  receiveTransfer,
  reserveTransfer,
  type TransferSink,
} from "@/lib/trace-transfer";
import type { PayloadAccess, TraceView } from "@/lib/trace-types";
const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
type PickerWindow = Window & {
  showSaveFilePicker?: (options: {
    suggestedName: string;
    types: { description: string; accept: Record<string, string[]> }[];
  }) => Promise<FileSystemFileHandle>;
};
export function TraceTransfer({
  access,
  view,
  text,
  readable,
}: {
  access: PayloadAccess;
  view: TraceView;
  text: string | null;
  readable: boolean;
}) {
  const t = useT();
  const active = useRef<AbortController | null>(null);
  const [state, setState] = useState<{
    kind: "copy" | "download";
    bytes: number;
    total: number | null;
  } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const abort = () => active.current?.abort();
    const clear = onTraceAccessCleared(() => {
      abort();
      setNotice(null);
      setError(null);
    });
    window.addEventListener("pagehide", abort);
    return () => {
      clear();
      window.removeEventListener("pagehide", abort);
      abort();
    };
  }, []);
  const run = async (kind: "copy" | "download") => {
    if (active.current) return;
    let release: (() => void) | undefined;
    let sink: TransferSink | undefined;
    const controller = new AbortController();
    try {
      release = reserveTransfer();
      active.current = controller;
      setNotice(null);
      setError(null);
      setState({ kind, bytes: 0, total: null });
      const picker = (window as PickerWindow).showSaveFilePicker;
      if (kind === "download" && picker) {
        const handle = await picker.call(window, {
          suggestedName: "observed-content.zip",
          types: [
            {
              description: "Observed content archive",
              accept: { "application/zip": [".zip"] },
            },
          ],
        });
        if (controller.signal.aborted)
          throw new DOMException("Transfer cancelled", "AbortError");
        const file = await handle.createWritable();
        sink = {
          write: (bytes) => file.write(bytes.slice().buffer),
          close: () => file.close(),
          abort: () => file.abort(),
        };
      }
      let lastProgress = -Infinity;
      const response = await traceAPI.transfer(
        access,
        view,
        kind,
        kind === "download" && !sink,
        controller.signal,
      );
      const result = await receiveTransfer(
        response,
        kind,
        controller.signal,
        (bytes, total) => {
          const now = performance.now();
          if (now - lastProgress >= 250 || bytes === total) {
            lastProgress = now;
            setState({ kind, bytes, total });
          }
        },
        sink,
      );
      if (controller.signal.aborted)
        throw new DOMException("Transfer cancelled", "AbortError");
      if (typeof result === "string")
        await navigator.clipboard.writeText(result);
      else if (result instanceof Blob) {
        const url = URL.createObjectURL(result);
        const link = document.createElement("a");
        link.href = url;
        link.download = "observed-content.zip";
        link.click();
        await new Promise<void>((resolve) =>
          setTimeout(() => {
            URL.revokeObjectURL(url);
            resolve();
          }, 0),
        );
      }
      setNotice(
        t(
          kind === "copy"
            ? "trace.copyComplete"
            : sink
              ? "trace.downloadComplete"
              : "trace.downloadReady",
        ),
      );
    } catch (error) {
      await sink?.abort().catch(() => {});
      if (error instanceof DOMException && error.name === "AbortError")
        setNotice(t("trace.transferCancelled"));
      else {
        if (
          error instanceof ApiError &&
          (error.status === 401 || error.status === 403)
        )
          clearTraceAccess();
        if (error instanceof ApiError && error.status === 410)
          invalidateTraceView();
        setError(error instanceof Error ? error.message : String(error));
      }
    } finally {
      if (active.current === controller) {
        active.current = null;
        setState(null);
      }
      release?.();
    }
  };
  const copyVisible = async () => {
    if (text === null) return;
    let release: (() => void) | undefined;
    try {
      release = reserveTransfer();
      await navigator.clipboard.writeText(text);
      setNotice(t("trace.copyComplete"));
      setError(null);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    } finally {
      release?.();
    }
  };
  return (
    <div className="mt-2 space-y-2">
      <div className="flex flex-wrap gap-2">
        {readable && (
          <button
            className={button}
            disabled={text === null || state !== null}
            onClick={() => void copyVisible()}
          >
            {t("trace.copyVisible")}
          </button>
        )}
        {readable && (
          <button
            className={button}
            disabled={state !== null}
            onClick={() => void run("copy")}
          >
            {t("trace.copyObserved")}
          </button>
        )}
        <button
          className={button}
          disabled={state !== null}
          onClick={() => void run("download")}
        >
          {t("trace.downloadObserved")}
        </button>
        {state && (
          <button className={button} onClick={() => active.current?.abort()}>
            {t("trace.cancelTransfer")}
          </button>
        )}
      </div>
      {state && (
        <div role="status" className="text-sm">
          <p>{t("trace.transferProgress", { bytes: state.bytes })}</p>
          <progress
            aria-label={t("trace.transferProgressLabel")}
            value={state.total ? state.bytes : undefined}
            max={state.total ?? undefined}
            className="max-w-full"
          />
        </div>
      )}
      {notice && (
        <p role="status" className="text-sm">
          {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="text-sm">
          {error}
        </p>
      )}
    </div>
  );
}
