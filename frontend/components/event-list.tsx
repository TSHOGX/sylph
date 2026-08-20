"use client";

import { memo, useMemo } from "react";
import type { StreamEvent } from "@/lib/api";
import { formatTime } from "@/lib/format";

/**
 * Per-kind presentation. Kinds come from the `sylph.runner.v1` vocabulary
 * (thinking, assistant, tool_call, tool_result, usage, status) plus the
 * server-authored `user` and `error` events. Unknown kinds are coerced to
 * `status` by the backend, so the fallback here is defensive only.
 */
const KIND_STYLES: Record<string, { box: string; label: string }> = {
  user: { box: "border-gray-300 bg-white", label: "You" },
  thinking: { box: "border-amber-200 bg-amber-50", label: "Thinking" },
  assistant: { box: "border-gray-200 bg-gray-50", label: "Assistant" },
  tool_call: { box: "border-blue-200 bg-blue-50", label: "Tool call" },
  tool_result: { box: "border-indigo-200 bg-indigo-50", label: "Tool result" },
  usage: { box: "border-gray-200 bg-white", label: "Usage" },
  status: { box: "border-emerald-200 bg-emerald-50", label: "Status" },
  error: { box: "border-red-200 bg-red-50", label: "Error" },
};

/**
 * Collapse a stream into what should be displayed.
 *
 * `delta` events carry only the newly arrived suffix and exist to make a live
 * stream feel immediate; the matching `completed` event carries the whole
 * value. Rendering both would show every token twice, so deltas are
 * accumulated under their `streamId` and replaced as soon as the `completed`
 * event for that id arrives.
 */
function collapse(events: StreamEvent[]): StreamEvent[] {
  const out: StreamEvent[] = [];
  // streamId -> index in `out` of the row currently representing that block.
  const openBlocks = new Map<string, number>();

  for (const event of events) {
    const key = event.streamId;
    if (event.phase === "delta" && key) {
      const at = openBlocks.get(key);
      if (at !== undefined) {
        const existing = out[at];
        out[at] = { ...existing, text: (existing.text ?? "") + (event.text ?? "") };
      } else {
        openBlocks.set(key, out.length);
        out.push({ ...event });
      }
      continue;
    }

    if (event.phase === "completed" && key && openBlocks.has(key)) {
      // The completed payload is authoritative: replace the accumulated text.
      out[openBlocks.get(key)!] = { ...event };
      openBlocks.delete(key);
      continue;
    }

    // `started` with no text yet would render an empty card; skip it and let
    // the first delta or the completed event create the row.
    if (event.phase === "started" && !event.text) {
      continue;
    }

    out.push(event);
  }

  return out;
}

export function EventList({ events }: { events: StreamEvent[] }) {
  const rows = useMemo(() => collapse(events), [events]);

  return (
    <ol className="space-y-2">
      {rows.map((event, index) => (
        <li key={`${event.seq}-${event.streamId ?? index}`}>
          <MemoizedEventCard event={event} />
        </li>
      ))}
    </ol>
  );
}

/**
 * Deltas arrive at token rate, so an unmemoized list re-renders every prior
 * card on each token — cost grows with conversation length exactly when the UI
 * most needs to stay responsive. Only the streaming tail changes identity, so
 * comparing by identity keeps completed rows untouched.
 *
 * This is deliberately not windowing: it costs no bundle bytes, where a
 * virtualization library would consume most of the remaining first-load budget.
 * Revisit if profiling shows DOM size, rather than re-render count, dominating.
 */
const MemoizedEventCard = memo(EventCard, (prev, next) => prev.event === next.event);

export function EventCard({ event }: { event: StreamEvent }) {
  const style = event.isError
    ? KIND_STYLES.error
    : (KIND_STYLES[event.kind] ?? { box: "border-gray-200 bg-white", label: event.kind });

  const streaming = event.phase === "delta" || event.phase === "started";
  const hasData = event.data != null && Object.keys(event.data as object).length > 0;

  return (
    <article className={`rounded-lg border p-3 ${style.box}`}>
      <header className="mb-1 flex items-baseline justify-between gap-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-gray-600">
          {style.label}
          {event.name && (
            <span className="ml-1.5 font-mono text-[11px] normal-case text-gray-500">
              {event.name}
            </span>
          )}
          {streaming && (
            <span className="ml-1.5 font-normal normal-case text-gray-400">
              streaming...
            </span>
          )}
        </h3>
        <time
          className="shrink-0 text-[11px] text-gray-400"
          dateTime={new Date(event.createdAtMs).toISOString()}
        >
          {formatTime(event.createdAtMs)}
        </time>
      </header>

      {event.text && (
        <p className="whitespace-pre-wrap break-words text-sm text-gray-900">
          {event.text}
        </p>
      )}

      {hasData && (
        <details className="mt-2">
          <summary className="cursor-pointer text-xs text-gray-500 hover:text-gray-900">
            Details
          </summary>
          <pre className="mt-1.5 overflow-x-auto rounded bg-white/70 p-2 text-[11px] leading-relaxed text-gray-800">
            {JSON.stringify(event.data, null, 2)}
          </pre>
        </details>
      )}
    </article>
  );
}
