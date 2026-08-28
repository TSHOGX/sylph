import { describe, expect, it } from "vitest";

import { decodeSessionId } from "./use-session-id-param";

/**
 * The decode that was written out three times and failed differently each time.
 *
 * Asserted on `decodeSessionId` rather than on the hook because the decode is the part
 * that was wrong, and a test that has to mount a component and stub a router to reach it
 * is a test nobody writes. The hook is one `typeof` check over this function.
 *
 * The case these cannot cover is the sidebar's: a comparison that is silently always
 * false has no unit-test signature — reading the param raw yields a *string*, which every
 * assertion here would accept. That one is pinned end to end in `workspace-shell.spec.ts`,
 * asserting the active row is actually marked, because only a rendered tree can observe
 * two id spaces failing to meet.
 */
describe("decodeSessionId", () => {
  it("recovers the identity triple from an encoded path segment", () => {
    // What the App Router hands back, and what the backend refuses: its `decode` splits on
    // a *literal* `:` and never percent-decodes, so the raw form names no session.
    expect(decodeSessionId("local%3Ae2e%3Ae2e-native-session")).toBe(
      "local:e2e:e2e-native-session",
    );
  });

  it("recovers a native id containing a slash, which a delegated session has", () => {
    // `claude` names a delegated session `<parent>/agent-a<name>-<hash>`, so the native
    // part carries a `/` that has to survive one path segment rather than becoming three.
    expect(
      decodeSessionId("env7fk2%3Aclaude%3A9bdacb6e%2Fagent-aExplore-1f2e3d"),
    ).toBe("env7fk2:claude:9bdacb6e/agent-aExplore-1f2e3d");
  });

  it("recovers a native id containing a literal percent", () => {
    // The case that makes the decode load-bearing rather than cosmetic: a `%` in a vendor
    // id is itself escaped as `%25`, so a value passed through un-decoded reaches
    // `segment()` and is mangled beyond recovery.
    expect(decodeSessionId("local%3Aclaude%3Aa%2520b")).toBe("local:claude:a%20b");
  });

  it("leaves an already-decoded id unchanged, so a double decode is not a corruption", () => {
    // Not a licence to call it twice — it is called once, at the route boundary — but the
    // property is worth pinning: a triple with no escapes is its own decoding, so the
    // helper is safe to adopt at a call site whose current state is unknown.
    expect(decodeSessionId("local:e2e:plain")).toBe("local:e2e:plain");
  });

  it("returns a malformed escape as-is rather than throwing", () => {
    // `decodeURIComponent("%")` raises `URIError`, and a truncated link or a hand-typed
    // URL produces exactly that. Throwing would replace a page reporting "no such
    // session" with a render crash, which says strictly less: the id is wrong either way,
    // and the backend's own error names which of the three parts failed to parse.
    expect(decodeSessionId("local%3Ae2e%3Abroken%")).toBe("local%3Ae2e%3Abroken%");
    expect(decodeSessionId("%")).toBe("%");
  });

  it("round-trips whatever encodeURIComponent produces", () => {
    // The two halves are one rule (`segment()` in `lib/api.ts` is the producer), so the
    // property that matters is that they compose back to the original — including the
    // characters an id may actually hold.
    for (const id of [
      "local:e2e:e2e-native-session",
      "env7fk2:claude:9bdacb6e/agent-aExplore-1f2e3d",
      "local:claude:a%20b",
      "local:pi:anthropic/claude-opus-5",
    ]) {
      expect(decodeSessionId(encodeURIComponent(id))).toBe(id);
    }
  });
});
