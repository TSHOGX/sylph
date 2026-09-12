/**
 * What the start screen shows in its one variable slot.
 *
 * The screen is otherwise static — identity, the two create actions, and the
 * shortcut hints render the same on every visit, which is what makes it paint
 * before any request settles. Only the middle slot depends on data, and two
 * decisions there are worth isolating from JSX:
 *
 * "sessions exist" and "sessions have not loaded yet" both look like an empty
 * array, and rendering the first-run guide for the second is a regression a
 * returning user meets on every navigation to `/`.
 *
 * And the resume list's unit is the directory, not the session, which is a grouping
 * with enough edge cases (path spellings, recency across groups, the cap applying
 * to groups rather than rows) to be worth asserting directly.
 */

import type { Session } from "@/lib/api-types";

/**
 * One row in the resume list: a directory, and the session to reopen in it.
 *
 * The unit is the directory rather than the session, because resuming work means
 * returning to a *place*. A directory with seven sessions produced seven rows that
 * differed only by timestamp — and for untitled sessions, which is most of them,
 * the rows were byte-identical apart from that. Five such rows are one destination
 * wearing five labels, and they crowd out the four other places worth offering.
 *
 * The newest session in a directory is the one to resume; reaching an older one is
 * the sidebar's job, and it lists all of them under that directory already.
 */
export interface RecentWorkspace {
  /** Absolute path. Display shortens it; the row's link needs the session. */
  workdir: string;
  /** Opaque environment owning this path. */
  environment?: string | null;
  /** Newest session here — the one the row opens. */
  session: Session;
  /** Other sessions in this directory, not counting `session`. */
  otherCount: number;
}

export type StartState =
  /** The listing has not settled. Render nothing here rather than guessing. */
  | { kind: "pending" }
  /** No session has ever been created in this app: the onboarding case. */
  | { kind: "firstRun" }
  /** Most recently used directories, ready to resume. Never empty. */
  | { kind: "recents"; workspaces: RecentWorkspace[] };

/** Rows in the resume list. */
export const RECENTS_LIMIT = 5;

/**
 * Choose the slot's content.
 *
 * `pending` is checked before emptiness, so a slow backend produces a quiet gap
 * rather than an onboarding guide that is wrong as soon as the data lands. An
 * error is treated as `pending` by the caller for the same reason: failing to
 * load sessions is not evidence that none exist, and the sidebar already reports
 * the failure with a retry.
 */
export function selectStartState(
  sessions: Session[] | undefined,
  { pending, limit = RECENTS_LIMIT }: { pending: boolean; limit?: number },
): StartState {
  if (pending || !sessions) return { kind: "pending" };
  if (sessions.length === 0) return { kind: "firstRun" };
  return { kind: "recents", workspaces: recentWorkspaces(sessions, limit) };
}

/**
 * Group sessions by directory, newest first, capped at `limit` directories.
 *
 * Trailing slashes are normalized so `/tmp` and `/tmp/` are one place rather than
 * two rows for the same directory — the backend does not guarantee a spelling, and
 * the sidebar normalizes the same way for the same reason.
 *
 * Recency of a directory is the newest session in it, so a directory worked in this
 * morning outranks one with more sessions from last month. Sorted from a copy: the
 * input array belongs to the query cache the sidebar also reads, and sorting it in
 * place would reorder what every other observer sees.
 */
export function recentWorkspaces(
  sessions: Session[],
  limit = RECENTS_LIMIT,
): RecentWorkspace[] {
  const byPath = new Map<string, RecentWorkspace>();

  for (const session of [...sessions].sort((a, b) => b.updatedAtMs - a.updatedAtMs)) {
    const workdir = session.workdir.replace(/\/+$/, "") || "/";
    const key = `${session.environment}\u0000${workdir}`;
    const existing = byPath.get(key);
    // The first session seen for a path is its newest, because the iteration is
    // already in descending order.
    if (existing) existing.otherCount += 1;
    else byPath.set(key, { workdir, environment: session.environment, session, otherCount: 0 });
  }

  return [...byPath.values()].slice(0, limit);
}
