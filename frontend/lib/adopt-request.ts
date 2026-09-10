/**
 * What a client asks for when it brings in a session an agent already has.
 *
 * # Why this is a module rather than an object literal at the call site
 *
 * `watch` is the field whose omission is invisible. It is optional on the wire and
 * defaults to `false` server-side (`docs/api/sessions.md` § `POST /api/v1/sessions/adopt`),
 * so a request that forgets it is accepted, answers `201`, and yields a session that reads
 * as complete and then never changes again. Nothing fails; the transcript just stops where
 * the snapshot did while the terminal goes on writing to the same file.
 *
 * That is not a shape a rendered-surface test can see — the flag is in a request body, and
 * the picker's own specs assert on rows and navigation. A pure builder is the honest seam:
 * `adopt-request.test.ts` reads the payload directly, so the assertion is about what is
 * sent rather than about what the client happened to render afterwards.
 *
 * The policy this encodes, and the two alternatives rejected for it, live beside the
 * surface that acts on it — `components/adopt-picker.tsx` § Adopting follows the store.
 */

import type { AdoptSessionRequest, NativeSessionRow } from "./api-types";

/**
 * The adopt request for one enumerated row.
 *
 * Takes the row rather than its parts, so identity travels as the unit the listing
 * reported it in: `(environment, agent, nativeId)` is one triple, and splitting it across
 * arguments is how an agent from one row reaches the endpoint beside a native id from
 * another.
 *
 * Everything policy-relevant — the working directory, the title — is derived from the
 * vendor's own store by the backend and deliberately not sent.
 */
export function adoptRequest(
  environment: string,
  row: Pick<NativeSessionRow, "agent" | "nativeId">,
): AdoptSessionRequest {
  return {
    environment,
    agent: row.agent,
    nativeId: row.nativeId,
    // Always followed, never conditional. A wrong `false` is a silently stale session; a
    // wrong `true` is a follower on a file that never changes, which emits nothing.
    watch: true,
  };
}
