"use client";

/**
 * `Cmd+K`: find a session and jump to it.
 *
 * **One store, so one kind of result.** Search and listings both name durable sessions
 * from the server-owned store. A hit can simply be opened; there is no import step or
 * second id space to reconcile.
 *
 * What is worth recording is that the deduplication was not merely redundant, it was
 * a hazard: it compared a live row's remembered archive id against a hit's id across
 * two disjoint id spaces, and a wrong comparison there produces **no error at any
 * layer** — the sets simply never intersect, every component behaves impeccably, and
 * the feature silently shows one conversation twice. With identity as
 * `(environment, agent, nativeId)` throughout, that comparison cannot be written.
 *
 * Two sources still feed the list, but they are the same *kind* of row and are
 * combined rather than ranked apart:
 *
 * - **The cached listing, filtered locally.** Instant, and it is what makes an empty
 *   query useful — the recent sessions, which is what a picker opened by muscle memory
 *   wants.
 * - **`GET /search`, for text inside transcripts the browser never loaded.** Debounced
 *   and gated on a real query, because FTS5 is orders of magnitude slower than every other read here
 *   is sub-millisecond. That is not politeness: bound to keystrokes it is a load
 *   generator.
 *
 * A session matched locally *and* by the index is one row, deduplicated by id — which
 * is now a safe comparison, since both sides carry the same id.
 */

import { useRouter } from "next/navigation";
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type Session } from "@/lib/api";
import { formatRelative, sessionLabel, shortenHome } from "@/lib/format";
import { liveSessionsQuery } from "@/lib/sessions-query";
import { workspacesQuery } from "@/lib/workspaces-query";
import { relayName, useEnvironments } from "@/lib/environments";
import { fuzzyRank } from "@/lib/fuzzy";
import { useDebounced } from "@/lib/use-debounced";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { useT } from "@/lib/i18n";
import { EnvironmentChip, FidelityMarker } from "@/components/badges";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";

/** Rows with no query: recent sessions, which is what muscle memory wants. */
const RECENT_ROWS = 8;

/** Cap per source, so one cannot bury the other. */
const PER_SOURCE = 12;

/** One row: a session, plus the transcript excerpt that matched it, when one did. */
interface Hit {
  session: Session;
  /** Only ever set by the index — a local match is on the label, which is visible. */
  excerpt?: string;
}

export function SearchOverlay() {
  const t = useT();
  const router = useRouter();
  const { closeOverlay } = useWorkspaceUI();

  /**
   * The query lives here, not in the list.
   *
   * It has to: it drives a debounced request to the backend, so this component reads
   * it, and the ranking below is the caller's own for the reason spelled out at
   * `useSelectList`'s `external` option — index hits are ordered by a full-text score
   * over transcripts the browser never loaded. The cursor still belongs to the list,
   * which is the half that had two owners before.
   */
  const [query, setQuery] = useState("");
  const debounced = useDebounced(query);

  // The same declaration the sidebar and the start screen use, so opening search
  // reads their cache entry instead of issuing a listing of its own. This used to
  // hand-write the key and the limit, which deduped only as long as both spellings
  // happened to agree.
  const sessionsQuery = useQuery(liveSessionsQuery());

  const workspacesResult = useQuery(workspacesQuery());

  const environments = useEnvironments();

  // Gated on a query worth a full-text scan. Two characters rather than one,
  // matching `docs/api/sessions.md` § Search: a single letter matches most of a real
  // store and answers a question nobody asked.
  const indexQuery = useQuery({
    queryKey: ["search", debounced],
    queryFn: () => api.search(debounced, PER_SOURCE),
    enabled: debounced.trim().length >= 2,
  });

  const hits = useMemo<Hit[]>(() => {
    const sessions = sessionsQuery.data?.sessions ?? [];
    const local: Hit[] = (
      query.trim()
        ? fuzzyRank(sessions, query, (session) => [
            sessionLabel(session),
            session.agent,
            session.workdir,
          ]).map((result) => result.item)
        : sessions.slice(0, RECENT_ROWS)
    )
      .slice(0, PER_SOURCE)
      .map((session) => ({ session }));

    // Deduplicated by session id, which is the comparison that used to be
    // unwritable. Local rows come first and win, because a row already in cache
    // carries the same session and its label matched something the user can see.
    const seen = new Set(local.map((hit) => hit.session.id));
    const indexed: Hit[] = (indexQuery.data?.results ?? [])
      .filter((result) => !seen.has(result.session.id))
      .slice(0, PER_SOURCE)
      .map((result) => ({
        session: result.session,
        excerpt: result.bestMatch?.excerpt?.replace(/\s+/g, " "),
      }));

    return [...local, ...indexed];
  }, [sessionsQuery.data, indexQuery.data, query]);

  const home = workspacesResult.data?.home;

  const items = useMemo<SelectChoice[]>(
    () =>
      hits.map(({ session, excerpt }) => ({
        label: sessionLabel(session),
        detail: `${session.agent}${
          session.workdir ? ` · ${shortenHome(session.workdir, home)}` : ""
        }`,
        // Why this row is here at all, when the match was inside the transcript: the
        // index matched words the title does not contain, so without the snippet the
        // row is a label that has nothing to do with what was typed.
        excerpt,
        meta: (
          <span className="flex items-center gap-1.5">
            <FidelityMarker value={session.fidelity} />
            {environments.showEnvironment && (
              <EnvironmentChip
                name={environments.nameOf(session.environment) ?? session.environment}
                reachableVia={relayName(session.environment, environments)}
              />
            )}
            {formatRelative(session.updatedAtMs)}
          </span>
        ),
        // Every hit opens. There is no import step and no second destination, which
        // is what deleted this surface's mutation, its pending badge, and its error
        // footer along with the archive.
        onSelect: () => {
          closeOverlay();
          router.push(`/sessions/${encodeURIComponent(session.id)}`);
        },
      })),
    [hits, home, environments, closeOverlay, router],
  );

  // `searchable: true` and `external`: the field is the surface's whole point, and the
  // ranking is this component's. No `onClose` — a row navigates, and Escape belongs to
  // `Overlay`.
  const list = useSelectList(items, {
    searchable: true,
    external: { query, setQuery },
  });

  return (
    <Overlay title={t("search.title")} onClose={closeOverlay} describedBy="search-hint">
      <SelectFilter
        list={list}
        name={t("search.results")}
        // Not "Filter results": this query reaches an index over transcripts that were
        // never sent to the browser, so it searches rather than narrows, and the name a
        // screen reader reads has to say which.
        label={t("search.field")}
        placeholder={t("search.placeholder")}
        size="roomy"
        trailing={
          indexQuery.isFetching ? (
            <span className="shrink-0 text-3xs text-ink-muted">
              {t("search.searching")}
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
        {/* A failed index read is reported rather than swallowed. It used to be an
            "archive unavailable" note, which was a different claim — that an optional
            external process was absent. This one is an actual failure, so it says so
            and the locally-matched rows above it are still usable. */}
        {indexQuery.error && (
          <span role="alert" className="text-warning">
            {t("search.indexFailed")}
          </span>
        )}
      </footer>
    </Overlay>
  );
}
