"use client";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useT } from "@/lib/i18n";
import { formatBytes } from "@/lib/format";
import { traceAPI } from "@/lib/trace-api";
import { onTraceAccessCleared } from "@/lib/trace-cache";
import {
  PlaybackClock,
  PLAYBACK_WINDOW,
  receiveEvidencePlayback,
  type PlaybackPolicy,
  type PlaybackRow,
} from "@/lib/trace-playback";
import type { TraceView } from "@/lib/trace-types";
import { MenuButton } from "./ui/menu";
const button =
  "tap-target min-h-11 rounded px-3 text-sm hover:bg-hover focus-ring disabled:opacity-50";
type Phase = "idle" | "playing" | "paused" | "stopped" | "completed" | "failed";

export function TracePlayback({
  environment,
  environmentName,
  view,
}: {
  environment: string;
  environmentName: string | null;
  view: TraceView;
}) {
  const t = useT();
  const active = useRef<{
    abort: AbortController;
    clock: PlaybackClock;
  } | null>(null);
  const [phase, setPhase] = useState<Phase>("idle"),
    [speed, setSpeed] = useState(1);
  const [received, setReceived] = useState(0);
  const [rows, setRows] = useState<PlaybackRow[]>([]),
    [seen, setSeen] = useState(0);
  const [policy, setPolicy] = useState<PlaybackPolicy | null>(null),
    [error, setError] = useState<string | null>(null);
  const [scope, setScope] = useState<{ name: string; cut: number } | null>(
    null,
  );
  const running = phase === "playing" || phase === "paused";
  const startButton = useRef<HTMLButtonElement | null>(null);
  const pauseButton = useRef<HTMLButtonElement | null>(null);
  const stopButton = useRef<HTMLButtonElement | null>(null);
  const restoreActionFocus = useRef(false);
  // Restore only a disappearing playback action; reading elsewhere keeps focus.
  const finish = useCallback((next: Phase) => {
    restoreActionFocus.current =
      document.activeElement === pauseButton.current ||
      document.activeElement === stopButton.current;
    setPhase(next);
  }, []);
  useLayoutEffect(() => {
    if (!running && restoreActionFocus.current) {
      restoreActionFocus.current = false;
      startButton.current?.focus();
    }
  }, [running]);

  useEffect(() => {
    const cancel = () => active.current?.abort.abort();
    const clear = onTraceAccessCleared(() => {
      active.current?.abort.abort(
        new DOMException("Playback access cleared", "SecurityError"),
      );
      setRows([]);
      setReceived(0);
      setSeen(0);
      setError(null);
      finish("idle");
    });
    window.addEventListener("pagehide", cancel);
    return () => {
      cancel();
      clear();
      window.removeEventListener("pagehide", cancel);
    };
  }, [finish]);
  const run = async () => {
    if (active.current) return;
    const abort = new AbortController(),
      clock = new PlaybackClock();
    active.current = { abort, clock };
    setPhase("playing");
    setRows([]);
    setReceived(0);
    setSeen(0);
    setError(null);
    setPolicy(null);
    setScope({ name: environmentName ?? environment, cut: view.evidenceCut });
    // Bound response setup too; the stream consumer also enforces the server's
    // resolved processing lifetime, including pauses after the response arrives.
    const timeout = setTimeout(
      () =>
        abort.abort(
          new DOMException("Playback duration exceeded", "TimeoutError"),
        ),
      300_000,
    );
    let count = 0,
      announced = 0,
      byteUpdate = 0,
      receivedBytes = 0;
    try {
      const response = await traceAPI.evidencePlayback(
        environment,
        view,
        speed,
        abort.signal,
      );
      await receiveEvidencePlayback(
        response,
        abort.signal,
        clock,
        setPolicy,
        (row) => {
          count++;
          setRows((previous) => [
            ...previous.slice(-(PLAYBACK_WINDOW - 1)),
            row,
          ]);
          if (performance.now() - announced >= 100) {
            setSeen(count);
            announced = performance.now();
          }
        },
        (bytes) => {
          receivedBytes = bytes;
          if (performance.now() - byteUpdate >= 100) {
            setReceived(bytes);
            byteUpdate = performance.now();
          }
        },
      );
      abort.signal.throwIfAborted();
      setSeen(count);
      setReceived(receivedBytes);
      finish("completed");
    } catch (reason) {
      if (
        abort.signal.aborted &&
        abort.signal.reason?.name === "SecurityError"
      ) {
        setRows([]);
        setReceived(0);
        setSeen(0);
        finish("idle");
      } else if (
        abort.signal.aborted &&
        abort.signal.reason?.name !== "TimeoutError"
      ) {
        setSeen(count);
        finish("stopped");
      } else {
        setRows([]);
        setReceived(0);
        setSeen(0);
        finish("failed");
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    } finally {
      abort.abort();
      clearTimeout(timeout);
      active.current = null;
    }
  };
  const toggle = () => {
    const paused = phase !== "paused";
    active.current?.clock.setPaused(paused);
    setPhase(paused ? "paused" : "playing");
  };
  return (
    <section
      className="space-y-3 border-t border-line pt-3"
      aria-label={t("trace.playback")}
    >
      <h4 className="font-medium">{t("trace.playback")}</h4>
      <p className="text-sm">
        {t("trace.playbackScope", {
          environment:
            (running ? scope?.name : null) ?? environmentName ?? environment,
          cut: (running ? scope?.cut : null) ?? view.evidenceCut,
        })}
      </p>
      <p className="text-sm text-muted">
        {t("trace.playbackPacing", {
          speed: (running ? policy?.speed : null) ?? speed,
          gap: (running ? policy?.maxGapMs : null) ?? 2000,
        })}
      </p>
      <div className="flex flex-wrap items-center gap-1">
        <MenuButton
          label={t("trace.playbackSpeed")}
          items={[1, 2, 4, 10].map((value) => ({
            label: `${value}×`,
            checked: value === speed,
            onSelect: () => setSpeed(value),
          }))}
        >
          {(trigger) => (
            <button
              {...trigger}
              className={button}
              disabled={running}
              aria-label={t("trace.playbackSpeed")}
            >
              {speed}×
            </button>
          )}
        </MenuButton>
        <button
          className={button}
          disabled={running}
          ref={startButton}
          onClick={() => void run()}
        >
          {t("trace.playbackStart")}
        </button>
        {running && (
          <>
            <button ref={pauseButton} className={button} onClick={toggle}>
              {t(
                phase === "paused"
                  ? "trace.playbackResume"
                  : "trace.playbackPause",
              )}
            </button>
            <button
              className={button}
              ref={stopButton}
              onClick={() => active.current?.abort.abort()}
            >
              {t("trace.playbackStop")}
            </button>
          </>
        )}
      </div>
      {phase !== "idle" && (
        <p role="status" className="text-sm">
          {t(`trace.playback_${phase}`, { count: seen })}
        </p>
      )}
      {phase !== "idle" && (
        <p className="text-sm text-muted">
          {t("trace.playbackBytes", { bytes: formatBytes(received) })}
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      {rows.length > 0 && (
        <>
          <p className="text-sm text-muted">
            {t("trace.playbackWindow", {
              count: rows.length,
              maximum: PLAYBACK_WINDOW,
              cut: scope?.cut ?? view.evidenceCut,
            })}
          </p>
          <ol
            aria-label={t("trace.playbackRows")}
            tabIndex={0}
            className="max-h-96 space-y-3 overflow-y-auto overscroll-contain focus-ring"
          >
            {rows.map((row) => (
              <li
                key={row.admission}
                className="min-w-0 rounded border border-line p-3 text-sm"
              >
                <p className="font-medium">
                  {t("trace.playbackAdmission", {
                    number: row.admission,
                    time: row.receivedAtMs,
                  })}
                </p>
                <p>{row.transport}</p>
                <p>
                  {t("trace.playbackGap", {
                    delta: row.recordedDeltaMs ?? "—",
                    wait: row.delayMs,
                  })}
                </p>
                {row.adjustment !== "recorded" &&
                  row.adjustment !== "first" && (
                    <p>
                      {t(
                        row.adjustment === "gap_clamped"
                          ? "trace.playbackClamped"
                          : "trace.playbackDiscontinuity",
                      )}
                    </p>
                  )}
                <p className="text-muted">
                  {t("trace.playbackPreview", {
                    bytes: row.previewBytes,
                    total: row.observedBytes,
                    encoding: row.encoding,
                  })}
                </p>
                {row.missingPrefix && <p>{t("trace.playbackMissingPrefix")}</p>}
                <pre className="mt-2 whitespace-pre-wrap break-all font-mono text-xs">
                  {row.preview}
                </pre>
                <dl className="mt-2 space-y-1 break-all text-xs text-muted">
                  <dt>{t("trace.playbackObservation")}</dt>
                  <dd>{row.observation}</dd>
                  <dt>{t("trace.playbackSource")}</dt>
                  <dd>{row.source}</dd>
                  <dt>{t("trace.playbackCapturedClock")}</dt>
                  <dd>{row.capturedAt}</dd>
                  <dt>{t("trace.playbackSourceClock")}</dt>
                  <dd>
                    {row.sourceClock === "null"
                      ? t("trace.unavailable")
                      : row.sourceClock}
                  </dd>
                </dl>
              </li>
            ))}
          </ol>
        </>
      )}
    </section>
  );
}
