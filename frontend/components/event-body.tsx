"use client";

import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type StreamEvent } from "@/lib/api";
import { useSessionIdParam } from "@/lib/use-session-id-param";

/** Keep the preview readable while the user decides whether to fetch its full body. */
export function EventBody({ event, children, label = "Load full content" }: {
  event: StreamEvent;
  children: (event: StreamEvent) => ReactNode;
  label?: string;
}) {
  return event.bodyRef
    ? <DeferredEventBody event={event} label={label}>{children}</DeferredEventBody>
    : children(event);
}

function DeferredEventBody({ event, children, label }: {
  event: StreamEvent;
  children: (event: StreamEvent) => ReactNode;
  label: string;
}) {
  const sessionId = useSessionIdParam();
  const [requested, setRequested] = useState(false);
  const query = useQuery({
    queryKey: ["session", sessionId, "body", event.bodyRef!.seq],
    queryFn: () => api.sessions.eventBody(sessionId!, event.bodyRef!.seq),
    enabled: requested && Boolean(sessionId),
    staleTime: Infinity,
    gcTime: 60_000,
    retry: false,
  });
  const complete = query.data ? { ...event, ...query.data, bodyRef: undefined } : event;
  const bytes = event.bodyRef!.textBytes + event.bodyRef!.dataBytes;

  return (
    <>
      {children(complete)}
      {!query.data && (
        <div className="py-1 text-2xs text-ink-muted">
          <button
            type="button"
            disabled={query.isFetching || !sessionId}
            onClick={() => query.isError ? void query.refetch() : setRequested(true)}
            className="rounded px-1 py-1 text-ink-soft underline decoration-line underline-offset-2 hover:bg-hover focus-ring disabled:opacity-60"
          >
            {query.isFetching ? "Loading full content..." : query.isError ? "Retry full content" : label}
          </button>
          <span className="ml-1">{bytes < 1_048_576 ? `${Math.ceil(bytes / 1024)} KB` : `${(bytes / 1_048_576).toFixed(1)} MB`}</span>
          {query.error && <p role="alert" className="mt-1 break-words text-danger">{query.error.message}</p>}
        </div>
      )}
    </>
  );
}
