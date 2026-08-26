/**
 * Folding one announced request row into the set a client is showing.
 *
 * # Why this is a module rather than four lines in the hook
 *
 * A pending request is durable, shared state — the protocol says so
 * (`docs/session-protocol.md` § requests): it is journaled, it outlives every
 * client, and *any* client may answer it. So the same question can be on screen in
 * two browsers and on a phone at once, and whoever answers first decides it for
 * everyone. That makes "what is this session blocked on" a value several
 * independent actors mutate, and the rule for folding an update into it is the kind
 * of thing that has to be pinned by tests rather than inlined into a stream reader
 * where only an end-to-end run can reach it.
 *
 * The bug that motivated it: the server announced a request's *arrival* and nothing
 * announced its *departure*, so a client's set could only grow. It shrank when a
 * 20-second poll happened to fire — and `refetchIntervalInBackground` is false by
 * default, so a backgrounded tab was not polling at all, while
 * `refetchOnWindowFocus: false` meant returning to it did not refetch either.
 * Answering on a phone therefore left the panel standing on the laptop until a
 * manual reload.
 *
 * # Why the row, and not a verb
 *
 * The frame carries the whole row, whose `status` says where it now stands, so this
 * reconciles against a *state* rather than replaying a vocabulary of transitions.
 * An `expired` status added later needs no change here: anything not `pending` is
 * not being waited on, which is the only distinction a client makes.
 */

import type { PendingRequest } from "@/lib/api";

/** Whether a row is still something the agent is blocked on. */
function isOpen(request: PendingRequest): boolean {
  return request.status === "pending";
}

/**
 * The set to show, given the set currently shown and one announced row.
 *
 * Pure, and total on its inputs: `current` may be undefined because the query has
 * not answered yet, in which case an open row still has to be adopted — a request
 * can be announced before the first `GET /requests` lands, and dropping it there
 * would strand the agent until the poll.
 *
 * Ordering is by `createdAtMs`, matching the server's own `ORDER BY created_at_ms`.
 * That matters because the dialog shows `requests[0]` and calls it "the one the
 * agent is actually blocked on": inserting an announced row at the end would let a
 * late frame for an *older* request jump the queue and answer the wrong question.
 */
export function mergeRequestRow(
  current: PendingRequest[] | undefined,
  incoming: PendingRequest,
): PendingRequest[] {
  const rest = (current ?? []).filter((request) => request.id !== incoming.id);

  // Departure. Removing it is the whole point of the announcement, and it is
  // deliberately not conditional on the row having been present: a client that never
  // saw the arrival must not resurrect it.
  if (!isOpen(incoming)) return rest;

  return [...rest, incoming].sort((a, b) => a.createdAtMs - b.createdAtMs);
}

/**
 * Parse an announced row, or `null` when the frame cannot be trusted.
 *
 * Deliberately strict about `id` and `status`, and deliberately silent on failure.
 * The invalidate that follows every application is the reconciler, so a frame this
 * rejects costs one refetch — whereas a half-parsed row applied to the cache would
 * show a dialog with no id to answer, which is worse than showing it a moment late.
 */
export function parseRequestFrame(data: string): PendingRequest | null {
  try {
    const parsed: unknown = JSON.parse(data);
    if (!parsed || typeof parsed !== "object") return null;
    const row = parsed as Partial<PendingRequest>;
    if (typeof row.id !== "string" || typeof row.status !== "string") return null;
    if (typeof row.createdAtMs !== "number") return null;
    return row as PendingRequest;
  } catch {
    return null;
  }
}
