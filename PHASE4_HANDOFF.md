# Phase 4 Handoff: Production Readiness

Phase 3 is complete. This document records what shipped, what is knowingly
deferred, and the two decisions that need a human.

## Phase 3 Closing State

Verified green at the close of Phase 3:

```sh
cd backend && cargo test    # 21 unit + 49 integration
cd backend && cargo clippy  # clean
cd frontend && npm run build
```

Live smoke test against the real archive (4702 sessions, 526995 events):
health OK with `tracedb.available=true`, 4 runners discovered, search returns
real hits, native import returns 201 with `origin=native`,
`identityMode=preserve`, `lifecycleMode=preserve` and an archive-derived
workdir, `?origin=native` filters correctly, `?origin=bogus` → 400.

## Known Divergence: trace-db Build vs Source

The single most important thing to carry forward.

`show` on the **installed** `trace-db` binary returns a bare event array. The
**current source** at `~/Workspace/trace-db` returns
`{session: {cwd, title, started_at_ms, ...}, mode, events}`. Two consequences:

1. `backend/src/tracedb/mod.rs` parses both shapes and prefers session
   metadata, falling back to scanning event text for a `cwd`. The metadata
   branch is **inert** until `trace-db` is rebuilt. It is covered by unit
   tests, not by live traffic.
2. Field spelling also differs. The installed build emits camelCase for
   `show`; current source has no `rename_all` on `Session` / `SessionTrace` /
   `Event`, so it emits snake_case. Every multi-word field we read carries a
   serde `alias` for both. Without it, `started_at_ms` would have silently
   parsed as `None` — a degradation with no error.

The event-text `cwd` scan exists only to compensate for metadata the old
response never carried. Do not delete it before the rebuild.

## Decision 1 (human, irreversible): Rebuild trace-db?

Rebuilding activates the metadata path and adds a `list` operation
(`API_OPERATIONS` in `main.rs` declares 5 ops; the installed binary serves 4).
Two cautions before anyone runs `cargo build --release` there:

- trace-db HEAD is `feat(config): add layered runtime configuration`. sylph-web
  spawns `trace-db api --db <path>`. Verify that flag still works after the
  config refactor, or the client goes dark.
- `store.rs` contains FTS rebuild and tokenizer migration code. Whether it
  auto-runs on open was **not** verified. Back up `trace.db` first.

## Decision 2 (product): native → sylph conversion shape

Converting a native session to a Sylph session is undefined in
`docs/api-design.md`. Two candidate shapes:

- `PATCH /api/v1/sessions/{id}` with `identityMode`/`lifecycleMode`
- `POST /api/v1/sessions/{id}/convert`

This is not a naming preference. Conversion is the one legitimate way to cross
the red line that forbids injecting `SOUL.md` into a native session, so the
endpoint's shape is the audit surface for that crossing. A dedicated verb makes
the crossing explicit and hard to do by accident; a `PATCH` field makes it a
field update indistinguishable from a rename. Recommend the dedicated verb, but
it needs a product call.

## Resolved During Phase 3

**Lineage variants are distinct imports, not duplicates.** Search returns
`<id>/agent-<hash>` variants as separate hits. Importing one returns 422
(no recorded workdir) rather than 409, confirming the archive treats it as its
own record. Search annotation therefore matches on exact trace id, not
`lineageRootId`. Collapsing lineage client-side would also cross the red line
that lineage folding and relevance ranking belong to TraceDB.

**Unresumable is not a bad request.** An archived session with no recorded
workdir now returns 422 `TRACE_SESSION_NOT_RESUMABLE` instead of 400. The
request was well-formed; the gap is in the archive, and the remedy is on the
archive side. Frontend copy names that remedy.

## Phase 4 Work Remaining

From the roadmap, in the order I would take them:

1. **README onboarding path** — one complete runnable sequence: install deps,
   start backend, start frontend, create a session, send one turn. This is an
   acceptance item, not documentation polish.
2. **Backend coverage to >80%** — `tracedb/mod.rs` and `error.rs` gained tests
   in Phase 3; measure before adding more, and prefer the thin modules.
3. **WCAG 2.1 AA audit** — 7 of the frontend files carry ARIA attributes
   already. Full validation needs assistive-technology testing and expert
   review; automated checks only narrow the field.
4. **E2E (Playwright) on critical paths** — create session, send turn, import
   native session, reconnect mid-stream.
5. **Performance** — bundle <200KB, LCP <2.5s, virtual scrolling for large
   trajectories. The archive has sessions approaching 1000 events, so virtual
   scrolling has a real target to test against.
6. **Non-functional** — crash recovery, concurrent access, SSE reconnection
   stress, memory leak checks.

Then Phase 5: packaging, LaunchAgent, install guide.

## Standing Constraints

- Bind to loopback. Non-loopback needs a separate auth and threat-model change.
- Never query TraceDB's SQLite directly; go through the JSON line protocol.
- Auth is a `SameSite=Strict` cookie, so the frontend must proxy `/api/v1/*`
  same-origin. `SYLPH_WEB_BACKEND` overrides the target.
- The token rotates on backend restart. A restart invalidates open browser
  sessions; that is intended.
