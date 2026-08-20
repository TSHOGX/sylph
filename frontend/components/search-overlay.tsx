"use client";

/**
 * `Cmd+K`: find a session and jump to it.
 *
 * Two sources, ranked separately because they answer different questions. Live
 * sessions are matched client-side: the list is already cached for the sidebar,
 * so filtering it is instant and works with the backend's archive unavailable.
 * The TraceDB archive is searched server-side, because full-text search over
 * durable history is exactly what it exists for and the events are not in the
 * browser.
 *
 * Live results come first. A session you can open beats one you would have to
 * import first.
 */

import { useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type SearchResult, type Session } from "@/lib/api";
import { formatRelative, sessionLabel } from "@/lib/format";
import { fuzzyRank } from "@/lib/fuzzy";
import { useDebounced } from "@/lib/use-debounced";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Overlay, scrollActiveIntoView, useListNavigation } from "@/components/overlay";
import { shortenHome } from "@/components/workspace-sidebar";

type Row =
  | { type: "live"; session: Session }
  | { type: "archive"; hit: SearchResult };

export function SearchOverlay() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { closeOverlay } = useWorkspaceUI();

  // One state object so a query change resets the cursor in the same update; two
  // separate states would need an effect to keep them consistent.
  const [{ query, active }, setPicker] = useState({ query: "", active: 0 });
  const listRef = useRef<HTMLDivElement>(null);
  const debounced = useDebounced(query);

  const sessionsQuery = useQuery({
    queryKey: ["sessions", { limit: 200 }],
    queryFn: () => api.sessions.list({ limit: 200 }),
  });

  const workspacesQuery = useQuery({
    queryKey: ["workspaces"],
    queryFn: () => api.workspaces.list(),
    staleTime: 60_000,
  });

  // The archive is only searched once the query is worth a subprocess spawn:
  // TraceDB search shells out, so firing it per keystroke would queue processes
  // faster than they finish.
  const archiveQuery = useQuery({
    queryKey: ["search", debounced],
    queryFn: () => api.search(debounced, 20),
    enabled: debounced.trim().length >= 2,
  });

  const rows = useMemo<Row[]>(() => {
    const sessions = sessionsQuery.data?.sessions ?? [];
    const live: Row[] = (
      query.trim()
        ? fuzzyRank(sessions, query, (session) => [
            sessionLabel(session),
            session.agent,
            session.workdir,
          ]).map((result) => result.item)
        : // No query: most recent sessions, which is the useful default for a
          // picker opened by muscle memory.
          sessions.slice(0, 8)
    ).map((session) => ({ type: "live" as const, session }));

    const archive: Row[] = (archiveQuery.data?.results ?? [])
      // A hit already imported is the same session as a live row, so showing it
      // twice would offer two ways to reach one conversation.
      .filter((hit) => !hit.sessionId)
      .map((hit) => ({ type: "archive" as const, hit }));

    return [...live.slice(0, 12), ...archive.slice(0, 12)];
  }, [sessionsQuery.data, archiveQuery.data, query]);

  // Clamped rather than reset in an effect: archive results arrive after the
  // query settles and can shrink the list under the cursor, and clamping during
  // render means Enter can never commit a row that no longer exists.
  const activeIndex = Math.min(active, Math.max(0, rows.length - 1));

  useEffect(() => scrollActiveIntoView(listRef.current, activeIndex), [activeIndex]);

  const setActive = useCallback(
    (index: number) => setPicker((current) => ({ ...current, active: index })),
    [],
  );

  const importSession = useMutation({
    mutationFn: (hit: SearchResult) =>
      api.sessions.import({ traceSessionId: hit.id, title: hit.title }),
    onSuccess: ({ session }) => {
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      closeOverlay();
      router.push(`/sessions/${session.id}`);
    },
  });

  function commit(index: number) {
    const row = rows[index];
    if (!row) return;
    if (row.type === "live") {
      closeOverlay();
      router.push(`/sessions/${row.session.id}`);
    } else {
      // Importing preserves the originating agent's identity and lifecycle; the
      // backend derives every policy field from the archive, so nothing here
      // needs to (or may) choose them.
      importSession.mutate(row.hit);
    }
  }

  const { onKeyDown } = useListNavigation(rows.length, commit);
  const home = workspacesQuery.data?.home;

  return (
    <Overlay title="Search sessions" onClose={closeOverlay} describedBy="search-hint">
      <div className="flex items-center gap-2 border-b border-gray-200 px-3 py-2.5">
        <span aria-hidden="true" className="text-gray-500">
          ⌕
        </span>
        <input
          type="text"
          role="combobox"
          aria-expanded
          aria-controls="search-results"
          aria-activedescendant={rows.length > 0 ? `search-row-${activeIndex}` : undefined}
          aria-label="Search sessions and archive"
          autoFocus
          value={query}
          onChange={(event) => setPicker({ query: event.target.value, active: 0 })}
          onKeyDown={(event) => onKeyDown(event, activeIndex, setActive)}
          placeholder="Search live sessions and the TraceDB archive..."
          className="flex-1 bg-transparent text-sm outline-none placeholder:text-gray-500"
        />
        {archiveQuery.isFetching && (
          <span className="text-[10px] text-gray-500">searching archive...</span>
        )}
      </div>

      <div
        ref={listRef}
        id="search-results"
        role="listbox"
        aria-label="Search results"
        className="min-h-0 flex-1 overflow-y-auto py-1"
      >
        {rows.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-gray-500">
            {query.trim() ? "No matching sessions." : "No sessions yet."}
          </p>
        ) : (
          rows.map((row, index) => (
            <SearchRow
              key={row.type === "live" ? row.session.id : `archive-${row.hit.id}`}
              row={row}
              index={index}
              active={index === activeIndex}
              home={home}
              onHover={() => setActive(index)}
              onSelect={() => commit(index)}
              importing={importSession.isPending && activeIndex === index}
            />
          ))
        )}
      </div>

      <footer
        id="search-hint"
        className="flex shrink-0 items-center justify-between gap-2 border-t border-gray-200 bg-gray-50 px-3 py-1.5 text-[10px] text-gray-500"
      >
        <span>↑↓ navigate · ↵ open · Esc close</span>
        {archiveQuery.data && !archiveQuery.data.available && (
          <span className="text-amber-700">
            Archive unavailable — live sessions only
          </span>
        )}
        {importSession.error && (
          <span role="alert" className="text-red-700">
            {importSession.error instanceof Error
              ? importSession.error.message
              : "Import failed."}
          </span>
        )}
      </footer>
    </Overlay>
  );
}

function SearchRow({
  row,
  index,
  active,
  home,
  onHover,
  onSelect,
  importing,
}: {
  row: Row;
  index: number;
  active: boolean;
  home?: string;
  onHover: () => void;
  onSelect: () => void;
  importing: boolean;
}) {
  const isLive = row.type === "live";
  const title = isLive ? sessionLabel(row.session) : (row.hit.title ?? row.hit.ask ?? row.hit.id);
  const path = isLive ? row.session.workdir : row.hit.cwd;
  const agent = isLive ? row.session.agent : row.hit.agent;
  const when = isLive ? row.session.updatedAtMs : row.hit.endedAtMs ?? row.hit.startedAtMs;
  const snippet = isLive ? undefined : row.hit.bestMatch?.snippet ?? row.hit.ask;

  return (
    <div
      id={`search-row-${index}`}
      data-index={index}
      role="option"
      aria-selected={active}
      onMouseEnter={onHover}
      onClick={onSelect}
      className={`cursor-pointer px-3 py-1.5 ${active ? "bg-gray-100" : ""}`}
    >
      <div className="flex items-baseline gap-2">
        <span
          aria-hidden="true"
          className={`shrink-0 text-[9px] ${
            isLive
              ? row.session.origin === "native"
                ? "text-violet-600"
                : "text-emerald-600"
              : "text-gray-500"
          }`}
        >
          {isLive ? (row.session.origin === "native" ? "◆" : "●") : "⌗"}
        </span>
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-gray-900">{title}</span>
        {!isLive && (
          <span className="shrink-0 rounded bg-gray-100 px-1 text-[9px] font-medium text-gray-600">
            {importing ? "importing..." : "archive"}
          </span>
        )}
        <span className="shrink-0 text-[10px] text-gray-600">{formatRelative(when)}</span>
      </div>
      <p className="truncate pl-4 text-[10px] text-gray-600">
        {agent}
        {path && ` · ${shortenHome(path, home)}`}
      </p>
      {snippet && (
        <p className="truncate pl-4 text-[10px] text-gray-600">{snippet.replace(/\s+/g, " ")}</p>
      )}
    </div>
  );
}
