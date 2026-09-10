import { describe, expect, it } from "vitest";
import { adoptRequest } from "./adopt-request";
import type { NativeSessionRow } from "./api-types";

/**
 * The adopt request's contents, asserted on the payload rather than on the surface.
 *
 * **The defect these pin is a request that succeeds.** `watch` is optional on the wire and
 * `false` by default, so a client omitting it gets a `201` and a session that is correct at
 * the instant it was parsed and never updates again — while the terminal it came from keeps
 * appending to the same file. There is no error to catch, no empty list, no failed
 * navigation: the transcript reads as finished. That is why the assertion is on the body
 * and not on anything the picker renders afterwards, which is identical either way.
 *
 * E2E cannot stand in for this. Its fixture agent has no native store and refuses `adopt`
 * on purpose, and even intercepting the POST would be asserting the flag through a browser
 * to learn something a pure function already decides.
 */

function row(over: Partial<NativeSessionRow> = {}): NativeSessionRow {
  return {
    agent: "claude",
    nativeId: "9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
    workdir: "/tmp/project",
    fidelity: "blocks",
    id: "env7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
    ...over,
  };
}

describe("adoptRequest", () => {
  it("asks to follow the store as it grows", () => {
    // The whole point of the endpoint's other mode is that it stops, so this is the field
    // that decides whether continuing the conversation in a terminal ever reaches Sylph.
    expect(adoptRequest("env7fk2", row()).watch).toBe(true);
  });

  it("sends watch explicitly rather than relying on a default", () => {
    // An absent field and `false` are the same request to the backend, so "present" is a
    // distinct assertion from "true": a builder that dropped the key would still satisfy a
    // loose equality check against `undefined`.
    expect(adoptRequest("env7fk2", row())).toHaveProperty("watch");
  });

  it("follows a session the listing reported as already ended", () => {
    // No branch on `endedAtMs`, deliberately. An absent end stamp is not an ended session,
    // and a session idle when listed and resumed in the terminal afterwards is exactly the
    // case a heuristic gets wrong — so the flag must not vary with either stamp.
    expect(adoptRequest("env7fk2", row({ endedAtMs: 1_760_000_000_000 })).watch).toBe(true);
    expect(adoptRequest("env7fk2", row({ endedAtMs: undefined })).watch).toBe(true);
  });

  it("carries the row's own identity triple", () => {
    // Identity is `(environment, agent, nativeId)`, and the row is the unit the listing
    // reported it in. A request mixing one row's agent with another's native id names a
    // session that does not exist.
    expect(adoptRequest("env9xk4", row({ agent: "codex", nativeId: "rollout-42" }))).toEqual({
      environment: "env9xk4",
      agent: "codex",
      nativeId: "rollout-42",
      watch: true,
    });
  });

  it("sends nothing the backend derives from the store", () => {
    // The working directory and the title come from the vendor's own record, and the
    // recorded workdir is bounded exactly like a created one. A client that sent either
    // would be choosing where an agent runs.
    const request = adoptRequest("env7fk2", row({ title: "Fix the SSE reconnect loop" }));
    expect(request).not.toHaveProperty("workdir");
    expect(request.title).toBeUndefined();
  });
});
