"use client";

/**
 * Adopt a session an agent already has: pick it from what the machine holds.
 *
 * # Why a picker rather than a field
 *
 * `POST /sessions/adopt` takes a vendor's own session id, and nobody knows one. A text
 * input would make the feature reachable only by copying a UUID out of `~/.claude/projects`
 * — which is the same defect the workdir field had before `WorkspacePicker`: an absolute
 * value typed from memory fails as a backend rejection, while a value chosen from what the
 * server enumerated cannot be mistyped.
 *
 * # A blocked row is shown, never hidden and never offered
 *
 * `GET /sessions/enumerate` marks a row it knows adopt would refuse, and this renders all
 * three reasons rather than filtering them out:
 *
 * - **Hiding an already-adopted session** leaves a user hunting for a session plainly on
 *   the machine, concluding Sylph cannot see it.
 * - **Offering one** hands out a `409` nobody can act on.
 *
 * So an `alreadyAdopted` row stays selectable and *navigates* to the session Sylph already
 * holds — the useful action, and the one a user actually wanted. The other two are
 * `disabled`, which renders dimmed and unfocusable, with `meta` saying why: `outsideRoots`
 * names a configuration fix and `noWorkdir` cannot be fixed at all.
 *
 * # Sizing, and the control this deliberately does not use
 *
 * A native session's label is frequently a whole sentence — the agent's own recorded title,
 * or the opening ask. **A `<select>` is sized by its longest option**, not by its current
 * value, and because that width comes from content the user cannot see, `min-w-0` and
 * `truncate` cannot recover it: the intrinsic minimum is already too large. That is what
 * pushed the composer off the right edge of a phone. So the agent filter is a row of
 * buttons over a fixed vocabulary, and the session list is `SelectRows`, which truncates.
 */

import { useMemo, useState } from "react";
import { useRouter } from "next/navigation";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type BlockedReason, type NativeSessionRow } from "@/lib/api";
import { workspacesQuery } from "@/lib/workspaces-query";
import { defaultEnvironment, useEnvironments } from "@/lib/environments";
import { shortenHome } from "@/lib/format";
import { useT, type Translate } from "@/lib/i18n";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import type { SelectChoice } from "@/lib/select-list";

/** How many rows one request asks for. */
const PAGE = 100;

export function AdoptPicker({ onClose }: { onClose: () => void }) {
  const t = useT();
  const router = useRouter();
  const queryClient = useQueryClient();
  const environments = useEnvironments();
  const environment = defaultEnvironment(environments.all);

  /**
   * Which agent to list, or every readable one.
   *
   * `null` is "all" rather than a sentinel string, so it cannot collide with an agent
   * named `all` — the vocabulary is the vendor's, not ours.
   */
  const [agent, setAgent] = useState<string | null>(null);

  /**
   * Pages already asked for, newest request last.
   *
   * Accumulated rather than replaced, because a filter should search everything loaded
   * rather than only the last page — and because `nextCursor` is opaque, so there is no
   * way to jump to a page except by walking.
   */
  const [cursors, setCursors] = useState<string[]>([]);

  const environmentId = environment?.id;

  const listing = useQuery({
    // The cursor list is part of the key, so paging is a new query rather than a mutation
    // of this one — which is what lets the previous page stay on screen while the next
    // loads instead of flashing an empty list.
    queryKey: ["native-sessions", environmentId, agent, cursors.length],
    queryFn: async () => {
      if (!environmentId) return { sessions: [], nextCursor: undefined };
      const pages = await Promise.all(
        [undefined, ...cursors].map((cursor) =>
          api.sessions.enumerate({
            environment: environmentId,
            agent: agent ?? undefined,
            limit: PAGE,
            cursor,
          }),
        ),
      );
      return {
        sessions: pages.flatMap((page) => page.sessions),
        nextCursor: pages[pages.length - 1]?.nextCursor,
      };
    },
    enabled: Boolean(environmentId),
    // A listing costs seconds on a cold snapshot for a whole machine,
    // so refetching on remount would make reopening the picker feel broken. The server's
    // own snapshot expires in 30 s, which bounds how stale this can be either way.
    staleTime: 30_000,
  });

  const adopt = useMutation({
    mutationFn: (row: NativeSessionRow) =>
      api.sessions.adopt({
        environment: environmentId!,
        agent: row.agent,
        nativeId: row.nativeId,
      }),
    onSuccess: ({ session }) => {
      // Both listings change: the sidebar gains a session, and the row it came from is now
      // `alreadyAdopted`.
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["native-sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      onClose();
      router.push(`/sessions/${encodeURIComponent(session.id)}`);
    },
    // Deliberately no `onError` beyond the default: the error is rendered below. A mutation
    // with no error path turns any backend refusal into a phantom UI bug — the cached list
    // is untouched, the control re-renders from it, and the only observable outcome is that
    // clicking did nothing.
  });

  /**
   * Which agents to offer as filters.
   *
   * Every agent the environment reports, unfiltered: `GET /environments` lists what that
   * machine can run and nothing else, so a second availability test here would be a
   * predicate over a set that is already the answer.
   *
   * A chip for an agent with **no** native store is still correct to show — `enumerate`
   * refuses it with a `400` naming what is readable, which is a better answer than an
   * ambiguous empty list. See `api::sessions::require_agents`.
   */
  const agents = useMemo(
    () => environment?.agents.map((agent) => agent.name) ?? [],
    [environment],
  );

  // Shared key with the sidebar, the history overlay, and `WorkspacePicker`, so all four
  // read one cache entry. Only `home` is wanted here — a native session's workdir is
  // frequently under it, and an unshortened absolute path is what makes a row unreadable
  // at a picker's width.
  const workspaces = useQuery(workspacesQuery());

  const rows = listing.data?.sessions;
  const home = workspaces.data?.home;

  const items = useMemo<SelectChoice[]>(() => {
    if (!rows) return [];
    return rows.map((row) => {
      const label = rowLabel(row, t);
      const blocked = row.blocked;
      return {
        label,
        // The path is the searchable line: a project is remembered by a middle segment
        // that no leaf name carries.
        detail: row.workdir ? shortenHome(row.workdir, home) : undefined,
        hint: row.agent,
        meta: rowMeta(row, t),
        // `alreadyAdopted` stays selectable and navigates; the other two cannot be acted
        // on at all, so they are dimmed and skipped by the cursor.
        disabled: blocked === "noWorkdir" || blocked === "outsideRoots",
        onSelect: () => {
          if (blocked === "alreadyAdopted") {
            onClose();
            router.push(`/sessions/${encodeURIComponent(row.id)}`);
            return;
          }
          if (blocked) return;
          adopt.mutate(row);
        },
      };
    });
  }, [rows, home, t, router, onClose, adopt]);

  // Always filterable, never `auto`: the threshold counts single-select rows and this list
  // marks none of them current, and a real machine holds hundreds to thousands — measured
  // 1000 / 1258 / 531 across the three agents — so the answer is known in advance.
  //
  // No `onClose`: this list does not own the surface. Escape bubbles to `Overlay`, which
  // owns dismissal; claiming it here would swallow the key (WCAG 2.1.2).
  const list = useSelectList(items, { searchable: true });

  return (
    <Overlay title={t("adopt.title")} onClose={onClose} describedBy="adopt-picker-hint">
      <SelectFilter
        list={list}
        name={t("adopt.filterName")}
        placeholder={t("adopt.filterPlaceholder")}
      />

      <div
        role="group"
        aria-label={t("adopt.agentFilterLabel")}
        className="flex shrink-0 flex-wrap gap-1 border-b border-line px-3 py-1.5"
      >
        <AgentChip active={agent === null} onClick={() => { setAgent(null); setCursors([]); }}>
          {t("adopt.allAgents")}
        </AgentChip>
        {agents.map((name) => (
          <AgentChip
            key={name}
            active={agent === name}
            onClick={() => { setAgent(name); setCursors([]); }}
          >
            {name}
          </AgentChip>
        ))}
      </div>

      {adopt.error && (
        <p role="alert" className="shrink-0 border-b border-danger-line bg-danger-bg px-3 py-1.5 text-xs text-danger">
          {adopt.error instanceof Error ? adopt.error.message : t("adopt.failed")}
        </p>
      )}

      <SelectScroll list={list} label={t("adopt.title")} fill>
        {listing.isPending ? (
          // Honest about the wait rather than a spinner: a cold listing is seconds, not
          // milliseconds, because two of the three adapters read more than a header per
          // file to avoid reporting an absent title or an absent end stamp.
          <output aria-live="polite" className="block px-3 py-6 text-center text-xs text-ink-muted">
            {t("adopt.loading")}
          </output>
        ) : listing.error ? (
          <div role="alert" className="px-3 py-6 text-center text-xs">
            <p className="font-medium text-danger">{t("adopt.loadFailed")}</p>
            <button
              type="button"
              onClick={() => void listing.refetch()}
              className="mt-1.5 rounded border border-danger-line px-2 py-1 font-medium text-danger hover:bg-danger-bg focus-ring"
            >
              {t("app.retry")}
            </button>
          </div>
        ) : items.length === 0 ? (
          // A machine with the CLIs installed and no sessions genuinely has none, and this
          // must read as "nothing here" rather than as a loading state or a failure.
          <p className="px-3 py-6 text-center text-xs text-ink-muted">{t("adopt.empty")}</p>
        ) : (
          <SelectRows list={list} />
        )}
      </SelectScroll>

      <footer
        id="adopt-picker-hint"
        className="flex shrink-0 items-center justify-between gap-2 border-t border-line bg-sunken px-3 py-1.5 pb-safe md:pb-1.5"
      >
        <span className="text-3xs text-ink-muted">
          {adopt.isPending ? t("adopt.adopting") : t("select.keys")}
        </span>
        {listing.data?.nextCursor && (
          <button
            type="button"
            onClick={() => setCursors((held) => [...held, listing.data!.nextCursor!])}
            disabled={listing.isFetching}
            className="shrink-0 rounded border border-line bg-raised px-2 py-0.5 text-3xs font-medium hover:bg-sunken focus-ring disabled:opacity-50"
          >
            {listing.isFetching ? t("adopt.loadingMore") : t("adopt.loadMore")}
          </button>
        )}
      </footer>
    </Overlay>
  );
}

/**
 * A filter chip.
 *
 * A `button` rather than a `<select>`, for the reason in this module's header: a select is
 * sized by its longest option and cannot be styled or truncated. `aria-pressed` rather than
 * `aria-selected`, because these are toggles rather than options in a listbox.
 */
function AgentChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`rounded border px-2 py-0.5 text-3xs font-medium focus-ring ${
        active
          ? "border-accent-line bg-accent-bg text-accent"
          : "border-line bg-raised hover:bg-sunken"
      }`}
    >
      {children}
    </button>
  );
}

/**
 * What to call a native session in a list.
 *
 * The vendor's own title when it kept one, and the native id otherwise. **The id is a
 * poor label and the honest one**: `codex` records no title for any session and `pi` for
 * none either, and the server's opening-ask fallback cannot reach their transcripts —
 * no adapter carries the user's typed text for `claude` or `codex`. Inventing something
 * friendlier here would be a guess rendered as a fact.
 */
function rowLabel(row: NativeSessionRow, t: Translate): string {
  if (row.title && row.title.trim()) return row.title;
  return t("adopt.untitled", { id: row.nativeId.slice(0, 8) });
}

/**
 * The trailing column: why a row is blocked, or how big it is.
 *
 * A reason outranks a size, because a row a user cannot act on needs to say so before it
 * says anything else.
 */
function rowMeta(row: NativeSessionRow, t: Translate): string | undefined {
  if (row.blocked) return blockedLabel(row.blocked, t);
  if (row.eventCount != null) return t("adopt.events", { count: row.eventCount });
  return undefined;
}

function blockedLabel(blocked: BlockedReason, t: Translate): string {
  switch (blocked) {
    case "alreadyAdopted":
      return t("adopt.blocked.alreadyAdopted");
    case "noWorkdir":
      return t("adopt.blocked.noWorkdir");
    case "outsideRoots":
      return t("adopt.blocked.outsideRoots");
  }
}
