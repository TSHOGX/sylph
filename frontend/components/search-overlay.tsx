"use client";
import { useT } from "@/lib/i18n";
import Link from "next/link";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { liveSessionsQuery } from "@/lib/sessions-query";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useTraceResource } from "@/lib/use-trace-resource";
import { rememberTraceNavigation } from "@/lib/trace-cache";
import { traceAPI } from "@/lib/trace-api";
import {
  agentName,
  agentUrl,
  historyLocationKey,
  type TraceView,
} from "@/lib/trace-types";
import { sessionLabel } from "@/lib/format";
import { Overlay } from "./overlay";

/** Global search consumes normalized, snapshot-scoped hits, never vendor event sequences. */
export function SearchOverlay() {
  const t = useT();
  const ui = useWorkspaceUI();
  const { closeOverlay } = ui;
  const [text, setText] = useState("");
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState<string | null>(null);
  const [searchView, setSearchView] = useState<TraceView | null>(null);
  const recent = useQuery(liveSessionsQuery());
  const result = useTraceResource(
    query ? `global-search:${query}:${cursor ?? ""}` : null,
    (signal) =>
      traceAPI.globalSearch(query, cursor, signal, searchView).then((page) => {
        if (!signal.aborted) setSearchView(page.view);
        return page;
      }),
    0,
    false,
  );
  return (
    <Overlay title={t("trace.findConversations")} onClose={closeOverlay} autoFocus>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          setQuery(text.trim());
          setCursor(null);
          setSearchView(null);
        }}
        className="flex gap-2 p-3"
      >
        <input
          aria-label={t("trace.searchConversations")}
          value={text}
          onChange={(event) => setText(event.target.value)}
          className="min-h-11 min-w-0 flex-1 rounded border border-line bg-canvas px-3 text-base"
          placeholder={t("trace.searchConversations")}
        />
        <button className="min-h-11 rounded border border-line px-3 focus-ring">
          {t("trace.search")}
        </button>
      </form>
      <div className="max-h-[65dvh] overflow-auto p-3">
        {query ? (
          <>
            {result.pending && (
              <p role="status">{t("trace.searchingHistory")}</p>
            )}
            {result.error && <p role="alert">{result.error.message}</p>}
            <ul className="space-y-3">
              {result.data?.items.map((hit) => (
                <li
                  key={`${hit.agent.address}:${hit.anchor}`}
                  className="rounded border border-line p-3"
                >
                  <h3 className="text-sm font-medium">
                    {agentName(hit.agent, t)}
                  </h3>
                  <p className="my-2 whitespace-pre-wrap text-sm">
                    {hit.excerpt}
                  </p>
                  <Link
                    className="inline-block min-h-11 py-2 text-sm underline focus-ring"
                    href={agentUrl(
                      hit.agent.address,
                      historyLocationKey(hit.history),
                      hit.anchor,
                    )}
                    onClick={() => {
                      if (ui.isMobile) ui.closeDrawer();
                      else ui.setRightOpen(false);
                      if (result.data)
                        rememberTraceNavigation(
                          hit.agent.address,
                          historyLocationKey(hit.history),
                          hit.anchor,
                          result.data.view,
                        );
                      closeOverlay();
                    }}
                  >
                    {t("trace.openResult")}
                  </Link>
                  <p className="text-xs text-ink-muted">
                    Matched {hit.history.label} at retained view{" "}
                    {result.data?.view.viewRevision}
                  </p>
                </li>
              ))}
            </ul>
            {result.data && (
              <p className="my-2 text-xs text-ink-muted">
                {result.data.total.scope} ·{" "}
                {result.data.total.complete
                  ? "Complete index scope"
                  : "Partial index coverage"}
              </p>
            )}
            <button
              className="min-h-11 rounded px-3 focus-ring"
              disabled={!result.data?.hasAfter || result.pending}
              onClick={() => setCursor(result.data?.afterCursor ?? null)}
            >
              {t("trace.moreSearch")}
            </button>
          </>
        ) : (
          <>
            <h3 className="mb-2 text-sm font-medium">{t("trace.recent")}</h3>
            <ul>
              {recent.data?.sessions.slice(0, 12).map((session) => (
                <li key={session.id}>
                  <Link
                    className="block min-h-11 rounded px-2 py-2 text-sm hover:bg-hover focus-ring"
                    href={agentUrl(session.traceAgentRef)}
                    onClick={closeOverlay}
                  >
                    {sessionLabel(session)}
                  </Link>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </Overlay>
  );
}
