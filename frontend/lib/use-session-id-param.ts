"use client";

/**
 * The session id named by the current route, decoded.
 *
 * # Why this exists at all: the App Router does not decode route params
 *
 * This is the fact a reader needs and cannot get from the documentation, so it is stated
 * here rather than at three call sites. Next's Pages router decodes every param —
 * `route-matcher.js` calls `decodeURIComponent` on each one — but that file is reached
 * only from `next-server.js`. The **App Router** does not: `useParams` returns
 * `PathParamsContext` as `getSelectedParams(tree)` built it, and nothing between the URL
 * and that context decodes anything. So `params.id` is the **raw** path segment.
 *
 * That matters here specifically because a session id is the identity triple
 * `<environment>:<agent>:<native_id>`, so it contains `:` — and its native part may
 * contain `/` and `%` besides, since `claude` names a delegated session
 * `<parent>/agent-a<name>-<hash>`. Every one of those characters has to be escaped to
 * survive a path segment, so the raw form is never the id: it is
 * `local%3Ae2e%3Ae2e-…`, and the triple has to be recovered before anything compares or
 * transmits it.
 *
 * # One rule in two functions, not four ad-hoc calls
 *
 * This is the consumer side of a round trip whose producers are already single-sourced:
 *
 * | Direction | Where | What it does |
 * | --- | --- | --- |
 * | id → URL | `workspace-sidebar.tsx`, `app/page.tsx` | `encodeURIComponent` on the href |
 * | id → wire | `segment()` in `lib/api.ts` | `encodeURIComponent` on the path segment |
 * | URL → id | **here** | `decodeURIComponent` on the route param |
 *
 * Keeping the decode in one place is not tidiness. It was written out three times and
 * **each copy failed differently**, which is the argument for a single producer:
 *
 * - `app/sessions/[id]/page.tsx` passed the raw value to `api.sessions.get`, which runs it
 *   through `segment()` — so `%3A` became `%253A`, the backend decoded once, saw `%3A`,
 *   and refused with "is not an encoded session id". A session that had been created
 *   successfully and was listed in the sidebar failed only when opened.
 * - `artifacts-pane.tsx` did the same on the file endpoints, so the working directory
 *   could not be listed. Same 400, different feature, and it read as a broken file pane.
 * - `workspace-sidebar.tsx` compared the raw param against `session.id` — the *decoded*
 *   triple — to decide which row is current. **That comparison was silently always
 *   false.** Nothing errored at any layer, no request was malformed, and the active row
 *   simply never highlighted. This is the failure `AGENTS.md` devotes a pitfall to: two id
 *   spaces for one thing, every component behaving impeccably, and the feature quietly
 *   doing nothing.
 *
 * The third is why this is a helper rather than three edits. The first two announce
 * themselves with a 400; that one is invisible, so the next raw read would reintroduce it
 * with nothing to catch it. `use-session-id-param.test.ts` pins the decode, and
 * `workspace-shell.spec.ts` asserts the highlight, because a unit test cannot observe a
 * comparison that is merely never true.
 *
 * # Not in `lib/api.ts`, deliberately
 *
 * `segment()` lives there and this is its natural pair, but `api.ts` imports nothing from
 * React and is not a client module — it is callable from anywhere, and putting a hook in
 * it would make the whole API client depend on a React runtime for one function. The
 * `lib/use-*.ts` convention already exists for exactly this (`use-dialog`, `use-session-trace`,
 * `use-agent-session`), so the pair is documented rather than co-located.
 */

import { useParams } from "next/navigation";

/**
 * Decode a route param that carries a session id.
 *
 * Exported separately from the hook so it is testable without a React tree and without a
 * router: the decode is the part that was wrong three times, and a test that has to mount
 * a component to reach it is a test nobody writes.
 *
 * A malformed escape is returned **as-is** rather than throwing.
 * `decodeURIComponent("%")` raises `URIError`, and a URL a user typed or a link that was
 * truncated can produce exactly that. Throwing here would replace a page reporting "no
 * such session" with a render crash, which is strictly less informative — the id is
 * already wrong either way, and the backend's own error names which of the three parts
 * failed to parse.
 */
export function decodeSessionId(raw: string): string {
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

/**
 * The decoded session id from the route, or `undefined` off a session route.
 *
 * `undefined` rather than an empty string, so a caller's `enabled: Boolean(id)` guard and
 * its query keys both behave: `""` is a value that would key a cache entry and read as
 * present.
 */
export function useSessionIdParam(): string | undefined {
  const params = useParams();
  return typeof params?.id === "string" ? decodeSessionId(params.id) : undefined;
}
