/**
 * What "refresh this transcript" means for one session.
 *
 * # Why this is a module rather than a condition at the call site
 *
 * Both of the decisions here fail *silently* when they are wrong, in opposite directions,
 * and neither is visible to a rendered-surface test.
 *
 * Skip the re-read and the control does nothing to the record: a session adopted from a
 * terminal can never gain a line the vendor appended afterwards, because invalidating a
 * query cache refetches what Sylph already holds. That is the defect this control shipped
 * with, and clicking it looks identical to a click that worked
 * (`docs/environments.md` § Re-reading a session Sylph already holds).
 *
 * Re-read a `stream` record and the cost is the mirror image: a native store never holds
 * `stream` fidelity, so parsing one can only add rows at a coarser granularity than the
 * record already has. The click appears to succeed either way.
 *
 * A pure function is the honest seam. The outcome is a decision, so a test can read the
 * decision; asserting through the menu would be asserting on markup that is the same
 * whichever branch ran.
 */

import type { Session } from "./api-types";

/**
 * Whether refreshing this session should ask the server to parse the vendor's store
 * again, or only re-read what Sylph already holds.
 *
 * **`stream` is the one record a re-read cannot help**, and skipping it is a statement
 * about resolution rather than a shortcut. `stream` is the highest fidelity there is and
 * a vendor's store never holds it, so re-parsing can only append at block or message
 * granularity — never improve the record (`crates/server/src/api/sessions.rs`). Such a
 * session is also live over SSE, so a cache invalidation is the whole of what a refresh
 * can mean for it.
 *
 * Takes the fidelity rather than the session, because that is the only field the decision
 * turns on and passing the whole row invites a second condition being added here later
 * that belongs to the caller.
 */
export function refreshRereadsTheStore(fidelity: Session["fidelity"]): boolean {
  return fidelity !== "stream";
}
