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
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type SearchResult, type Session } from "@/lib/api";
import { formatRelative, sessionLabel, shortenHome } from "@/lib/format";
import { liveSessionsQuery } from "@/lib/sessions-query";
import { fuzzyRank } from "@/lib/fuzzy";
import { useDebounced } from "@/lib/use-debounced";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useT } from "@/lib/i18n";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";
import {
  ArchiveIcon,
  OriginNativeIcon,
  OriginSylphIcon,
} from "@/components/icons";

type Row =
  | { type: "live"; session: Session }
  | { type: "archive"; hit: SearchResult };

export function SearchOverlay() {
  const t = useT();
  const router = useRouter();
  const queryClient = useQueryClient();
  const { closeOverlay } = useWorkspaceUI();

  /**
   * The query lives here, not in the list.
   *
   * It has to: it drives a debounced request to the backend, so this component reads
   * it, and the ranking below is the caller's own for the reason spelled out at
   * `useSelectList`'s `external` option — archive hits are ordered by a full-text
   * index over transcripts the browser never loaded. The cursor still belongs to the
   * list, which is the half that had two owners before.
   */
  const [query, setQuery] = useState("");
  const debounced = useDebounced(query);

  // The same declaration the sidebar and the start screen use, so opening search
  // reads their cache entry instead of issuing a listing of its own. This used to
  // hand-write the key and the limit, which deduped only as long as both spellings
  // happened to agree.
  const sessionsQuery = useQuery(liveSessionsQuery());

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

  const home = workspacesQuery.data?.home;
  // Which hit is being imported, read from the mutation's own variables rather than
  // from the cursor. The cursor was the wrong source twice over: hovering another row
  // moved the badge off the row that was actually importing, and deriving it from
  // `activeIndex` would now make the rows depend on the list that is built from them.
  const importingId = importSession.isPending ? importSession.variables?.id : undefined;
  // Destructured, because the mutation object is a fresh one every render and the memo
  // below would then rebuild every row on every keystroke. `mutate` is stable.
  const importHit = importSession.mutate;

  const items = useMemo<SelectChoice[]>(
    () =>
      rows.map((row) => {
        if (row.type === "live") {
          const session = row.session;
          return {
            label: sessionLabel(session),
            icon: (
              <span
                className={session.origin === "native" ? "text-native" : "text-success"}
              >
                {session.origin === "native" ? (
                  <OriginNativeIcon size={9} />
                ) : (
                  <OriginSylphIcon size={9} />
                )}
              </span>
            ),
            detail: `${session.agent}${
              session.workdir ? ` · ${shortenHome(session.workdir, home)}` : ""
            }`,
            meta: formatRelative(session.updatedAtMs),
            onSelect: () => {
              closeOverlay();
              router.push(`/sessions/${session.id}`);
            },
          };
        }

        const hit = row.hit;
        return {
          label: hit.title ?? hit.ask ?? hit.id,
          icon: (
            <span className="text-ink-muted">
              <ArchiveIcon size={12} />
            </span>
          ),
          detail: `${hit.agent}${hit.cwd ? ` · ${shortenHome(hit.cwd, home)}` : ""}`,
          // The snippet is why this row is here at all: TraceDB matched words in the
          // transcript, and without showing them an archive hit is a title that does
          // not contain the query.
          excerpt: (hit.bestMatch?.snippet ?? hit.ask)?.replace(/\s+/g, " "),
          meta: (
            <span className="flex items-center gap-1.5">
              <span className="rounded bg-hover px-1 font-medium text-ink-muted">
                {importingId === hit.id ? t("search.importing") : t("search.archiveBadge")}
              </span>
              {formatRelative(hit.endedAtMs ?? hit.startedAtMs)}
            </span>
          ),
          // Importing preserves the originating agent's identity and lifecycle; the
          // backend derives every policy field from the archive, so nothing here
          // needs to (or may) choose them.
          onSelect: () => importHit(hit),
        };
      }),
    [rows, home, importingId, closeOverlay, router, importHit, t],
  );

  // `searchable: true` and `external`: the field is the surface's whole point, and the
  // ranking is this component's. No `onClose` — a live row navigates and an archive
  // row starts a mutation that closes on success, so tearing the surface down here
  // would hide an import that can still fail. Escape belongs to `Overlay`.
  const list = useSelectList(items, {
    searchable: true,
    external: { query, setQuery },
  });

  return (
    <Overlay title={t("search.title")} onClose={closeOverlay} describedBy="search-hint">
      <SelectFilter
        list={list}
        name={t("search.results")}
        // Not "Filter search results": this query reaches TraceDB's index over
        // transcripts that were never sent to the browser, so it searches rather than
        // narrows, and the name a screen reader reads has to say which.
        label={t("search.field")}
        placeholder={t("search.placeholder")}
        size="roomy"
        trailing={
          archiveQuery.isFetching ? (
            <span className="shrink-0 text-3xs text-ink-muted">
              {t("search.searchingArchive")}
            </span>
          ) : undefined
        }
      />

      <SelectScroll list={list} label={t("search.results")} fill>
        {list.rows.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-ink-muted">
            {query.trim() ? t("search.none") : t("search.empty")}
          </p>
        ) : (
          <SelectRows list={list} />
        )}
      </SelectScroll>

      <footer
        id="search-hint"
        className="flex shrink-0 items-center justify-between gap-2 border-t border-line bg-sunken px-3 py-1.5 text-3xs text-ink-muted"
      >
        <span>{t("search.keys")}</span>
        {archiveQuery.data && !archiveQuery.data.available && (
          <span className="text-warning">{t("search.archiveUnavailable")}</span>
        )}
        {importSession.error && (
          <span role="alert" className="text-danger">
            {importSession.error instanceof Error
              ? importSession.error.message
              : t("search.importFailed")}
          </span>
        )}
      </footer>
    </Overlay>
  );
}
