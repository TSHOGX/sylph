"use client";
import { useEffect, useRef, useState } from "react";
import { useT } from "@/lib/i18n";
import { traceAPI } from "@/lib/trace-api";
import { reserveTransfer, type TransferSink } from "@/lib/trace-transfer";
import { onTraceAccessCleared } from "@/lib/trace-cache";
import { formatBytes } from "@/lib/format";
import type { TraceView } from "@/lib/trace-types";
const button =
  "min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
type FilePicker = Window & {
  showSaveFilePicker?: (options: { suggestedName: string }) => Promise<{
    createWritable: () => Promise<{
      write: (bytes: ArrayBuffer) => Promise<void>;
      close: () => Promise<void>;
      abort: () => Promise<void>;
    }>;
  }>;
};

export function TraceEvidenceExport({
  environment,
  environmentName,
  view,
}: {
  environment: string;
  environmentName: string | null;
  view: TraceView;
}) {
  const t = useT(),
    active = useRef<AbortController | null>(null);
  const [bytes, setBytes] = useState<number | null>(null),
    [error, setError] = useState<string | null>(null),
    [done, setDone] = useState(false);
  const [exportScope, setExportScope] = useState<{
    name: string;
    cut: number;
  } | null>(null);
  useEffect(() => {
    const abort = () => active.current?.abort();
    const clear = onTraceAccessCleared(() => {
      abort();
      setDone(false);
      setError(null);
    });
    window.addEventListener("pagehide", abort);
    return () => {
      abort();
      clear();
      window.removeEventListener("pagehide", abort);
    };
  }, []);
  const run = async () => {
    if (active.current) return;
    const controller = new AbortController();
    let release: (() => void) | undefined, sink: TransferSink | undefined;
    try {
      release = reserveTransfer();
      active.current = controller;
      setError(null);
      setDone(false);
      setBytes(0);
      setExportScope({
        name: environmentName ?? environment,
        cut: view.evidenceCut,
      });
      const picker = (window as FilePicker).showSaveFilePicker;
      if (picker) {
        const file = await (
          await picker.call(window, { suggestedName: "sylph-evidence.jsonl" })
        ).createWritable();
        sink = {
          write: (bytes) => file.write(bytes.slice().buffer),
          close: () => file.close(),
          abort: () => file.abort(),
        };
      }
      if (controller.signal.aborted)
        throw new DOMException("Export cancelled", "AbortError");
      const response = await traceAPI.evidenceExport(
        environment,
        view,
        !sink,
        controller.signal,
      );
      const { receiveEvidenceExport } = await import(
        "@/lib/trace-evidence-export"
      );
      let last = 0;
      const blob = await receiveEvidenceExport(
        response,
        controller.signal,
        (value) => {
          const now = performance.now();
          if (now - last > 100) {
            setBytes(value);
            last = now;
          }
        },
        sink,
      );
      if (blob) {
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = "sylph-evidence.jsonl";
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 0);
      }
      setDone(true);
    } catch (reason) {
      await sink?.abort().catch(() => {});
      if (!controller.signal.aborted)
        setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      release?.();
      active.current = null;
      setBytes(null);
      setExportScope(null);
    }
  };
  return (
    <section className="space-y-2 border-t border-line pt-3">
      <h4 className="font-medium">{t("trace.exportEvidence")}</h4>
      <p className="text-sm">
        {t("trace.exportEnvironmentScope", {
          environment: exportScope?.name ?? environmentName ?? environment,
          cut: exportScope?.cut ?? view.evidenceCut,
        })}
      </p>
      <button
        className={button}
        disabled={bytes !== null}
        onClick={() => void run()}
      >
        {t("trace.exportDownload")}
      </button>
      {bytes !== null && (
        <>
          <p role="status">
            {t("trace.exportProgress", { bytes: formatBytes(bytes) })}
          </p>
          <button className={button} onClick={() => active.current?.abort()}>
            {t("trace.cancelTransfer")}
          </button>
        </>
      )}
      {error && <p role="alert">{error}</p>}
      {done && <p role="status">{t("trace.exportDone")}</p>}
    </section>
  );
}
