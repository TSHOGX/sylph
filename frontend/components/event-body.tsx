"use client";

import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type StreamEvent } from "@/lib/api";
import { useSessionIdParam } from "@/lib/use-session-id-param";

/** Keep the preview readable while the user decides whether to fetch its full body. */
export function EventBody({
  event,
  children,
  bodyUrl,
  bodyVersion,
  label = "Load full content",
}: {
  event: StreamEvent;
  children: (event: StreamEvent) => ReactNode;
  label?: string;
  bodyUrl?: string;
  bodyVersion?: number;
}) {
  return event.bodyRef ? (
    <DeferredEventBody
      event={event}
      label={label}
      bodyUrl={bodyUrl}
      bodyVersion={bodyVersion}
    >
      {children}
    </DeferredEventBody>
  ) : (
    children(event)
  );
}

function DeferredEventBody({
  event,
  children,
  label,
  bodyUrl,
  bodyVersion,
}: {
  event: StreamEvent;
  children: (event: StreamEvent) => ReactNode;
  label: string;
  bodyUrl?: string;
  bodyVersion?: number;
}) {
  const sessionId = useSessionIdParam();
  const [requested, setRequested] = useState<{
    event: StreamEvent;
    bodyUrl?: string;
    version?: number;
  } | null>(null);
  const selected = requested?.event ?? event;
  const selectedUrl = requested?.bodyUrl ?? bodyUrl;
  const query = useQuery({
    queryKey: [
      "session",
      sessionId,
      "body",
      selected.bodyRef!.seq,
      selectedUrl,
      requested?.version,
    ],
    queryFn: async ({ signal }) => {
      if (!selectedUrl)
        return api.sessions.eventBody(
          sessionId!,
          selected.bodyRef!.seq,
          signal,
        );
      const response = await fetch(selectedUrl, {
        signal,
        credentials: "include",
      });
      if (!response.ok)
        throw new Error(`Could not load body (${response.status})`);
      return response.json() as Promise<{ text: string; data?: unknown }>;
    },
    enabled: Boolean(requested) && Boolean(sessionId),
    staleTime: Infinity,
    gcTime: 0,
    retry: false,
  });
  const complete = query.data
    ? { ...selected, ...query.data, bodyRef: undefined }
    : event;
  const changed =
    Boolean(requested) &&
    (requested?.version !== bodyVersion ||
      selected.bodyRef?.seq !== event.bodyRef?.seq);
  const bytes = event.bodyRef!.textBytes + event.bodyRef!.dataBytes;

  return (
    <>
      {children(complete)}
      {(!query.data || changed) && (
        <div className="py-1 text-2xs text-ink-muted">
          <button
            type="button"
            disabled={query.isFetching || !sessionId}
            onClick={() =>
              query.isError && !changed
                ? void query.refetch()
                : setRequested({ event, bodyUrl, version: bodyVersion })
            }
            className="rounded px-1 py-1 text-ink-soft underline decoration-line underline-offset-2 hover:bg-hover focus-ring disabled:opacity-60"
          >
            {query.isFetching
              ? "Loading full content..."
              : query.isError
                ? "Retry full content"
                : changed
                  ? "Load latest content"
                  : label}
          </button>
          <span className="ml-1">
            {bytes < 1_048_576
              ? `${Math.ceil(bytes / 1024)} KB`
              : `${(bytes / 1_048_576).toFixed(1)} MB`}
          </span>
          {query.error && (
            <p role="alert" className="mt-1 break-words text-danger">
              {query.error.message}
            </p>
          )}
        </div>
      )}
    </>
  );
}
