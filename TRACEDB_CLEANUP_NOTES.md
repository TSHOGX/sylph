# TraceDB Cleanup — 2026-08-20

## Summary

Removed backward-compatibility code from `backend/src/tracedb/mod.rs` and pinned
the wire protocol to the current trace-db release (verified by driving
`trace-db api` directly on 2026-08-20). Added configuration for Next dev
cross-origin access scenarios (cpolar tunnels, LAN addresses). Documented and
resolved the three reported access failures.

## What Changed

### Backend TraceDB Client (`backend/src/tracedb/mod.rs`)

**Removed:**
- Custom `Deserialize` impl that supported bare-array `show` responses (old
  build format)
- Dual-spelling serde aliases (`#[serde(alias = "...")]`) on every field, which
  masked protocol changes and let field renames pass silently as `None`
- Event-scanning fallback for `cwd` — measured across 400 sessions: 0/75
  sessions with no metadata `cwd` were rescued by scanning event JSON
- Unused `event_count` field in `SessionFacts`

**Pinned wire shapes:**
- `show` returns snake_case (`created_at_ms`, `started_at_ms`) because its
  payload comes from `model.rs` types with no `#[serde(rename_all)]`
- `stats`, `search`, and `list` return camelCase (`totalSessions`,
  `startedAtMs`, `lineageRootId`)

Tests now use payloads copied from live `trace-db api` output so a spelling
change fails the suite rather than degrading to `None`.

**Added:**
- `kind` filter to `show` requests so `session_facts()` fetches only user
  events (a dozen rows) instead of the full stream (up to thousands)
- `SessionTrace`, `TraceSession`, `TraceEvent` structs that match the exact
  snake_case spellings `show` emits
- `BestMatch` and `SearchHit` structs that match the exact camelCase `search`
  emits, omitting scoring internals (`score`, `scoreBreakdown`, `bm25`) that
  couple the API to TraceDB's ranking mechanics

**Verified metadata coverage:** Sampled 400 archived sessions:
- 19% (75/400) have no `cwd` — almost all from `pi` agent
- ~99% have no `title` (fallback to opening user text is normal)
- ~99% have no `started_at_ms` (fallback to first event's `created_at_ms`)

The metadata sparsity is genuine data characteristics, not a compatibility gap.

### Frontend Dev Origin Allowlist (`frontend/next.config.ts`)

Next 16 blocks cross-site requests to `/_next/*` dev resources as DNS-rebinding
defense. Requests from a cpolar tunnel or LAN IP fail with 403, freezing the
page at "Connecting to backend..." because React never hydrates.

**Added:** `SYLPH_WEB_DEV_ORIGINS` env var (comma-separated list of origins
allowed to fetch `/_next/*` chunks). Omit it for normal localhost dev.

Example:
```sh
export SYLPH_WEB_DEV_ORIGINS="https://44fb4bb.r8.cpolar.cn,http://100.125.12.35:3000"
npm run dev
```

### Test Harness (`backend/tests/common/mod.rs`, `tests/integration.rs`)

**Replaced `MockTraceDb::with_events`** with two constructors that match the
current protocol:
- `with_session(metadata, events)` → full `show` response
- `without_session()` → `null` (unknown id)

The mock now emits snake_case for `show` and camelCase for `search`, mirroring
the real API's per-operation inconsistency.

### Documentation Updates

**`AGENTS.md` pitfall:**
- Replaced "TraceDB builds disagree on JSON shape" with "TraceDB's API spellings
  are inconsistent per-operation" — the build is current, but `show` and
  `search` have different naming conventions by design.

**`docs/roadmap.md` open decision:**
- Removed "Rebuild trace-db?" (resolved: it was rebuilt 2026-08-20, the default
  database path works, and archive metadata is now available).

## Three Reported Access Failures

### 1. http://100.125.12.35:3000 → "This page isn't working"

**Root cause:** macOS system HTTP proxy at `127.0.0.1:17891` (likely Clash or
Surge) intercepts the CGNAT address `100.125.12.35` because its BypassList
covers private ranges (127.*, 10.*, 172.16-31.*, 192.168.*) but not the
`100.64.0.0/10` CGNAT block. The proxy returns 502.

**Fix (user-side):** Add `100.*` or `*.100.125.12.35` to the system proxy's
exception list in System Settings → Network → Advanced → Proxies.

**Also needs:** Set `SYLPH_WEB_DEV_ORIGINS=http://100.125.12.35:3000` so Next
allows `/_next/*` requests from that origin. Without it, the page loads but
chunks return 403, leaving React unable to hydrate.

### 2. localhost:3000 shows no password prompt and only one session

**Not a bug.** This is correct behavior by design:

- **No password:** Auth is cookie-based with automatic bootstrap for loopback
  clients. The backend hands out an auth cookie to any `127.0.0.1` or
  `localhost` request, so browser sessions on the same machine never prompt.
- **One session:** The journal database (`live.db`) genuinely has one session.
  The 4,705 archived sessions live in TraceDB and appear only under the Search
  tab (not the home session list). Importing a native session from the archive
  adds it to the live journal, where it becomes listable and resumable.

This is the intended design. A future UX enhancement could surface recent
archived sessions on the home page or add a "Browse Archive" section, but
that's not in scope for this cleanup.

### 3. cpolar tunnel stuck at "Connecting to backend..."

**Root cause:** Next dev blocks `/_next/*` chunk requests from the cpolar origin
(`https://44fb4bb.r8.cpolar.cn`) with 403 because it's not in the allowlist.
Without the client-side JS, React never hydrates and the server-rendered
"Connecting to backend..." message stays frozen.

**Fix:** Set `SYLPH_WEB_DEV_ORIGINS=https://44fb4bb.r8.cpolar.cn` and restart
the dev server. The cpolar hostname changes when the tunnel rotates, so either
update the env var when it changes or use the wildcard `*.r8.cpolar.cn` if the
tier supports custom domains.

**cpolar's own basic auth (401) is separate:** The user already passed that (the
page shell loads). The stuck message is purely a Next dev cross-origin block.

**Backend Host check is not the issue:** Next's rewrite proxy swaps the `Host`
header to the destination (`localhost:4317`), so backend auth works fine
regardless of the incoming cpolar hostname.

## TraceDB API Inconsistency (Flagged for Trace-DB Project)

The API spellings are inconsistent across operations:

| Operation | Casing         | Example Fields                          |
|-----------|----------------|-----------------------------------------|
| `show`    | snake_case     | `created_at_ms`, `started_at_ms`, `idx` |
| `stats`   | camelCase      | `totalSessions`, `totalEvents`          |
| `search`  | camelCase      | `startedAtMs`, `lineageRootId`          |
| `list`    | camelCase      | `startedAtMs`, `ingestedAtMs`           |

This is because `show`'s payload is built from `model.rs` types that carry no
`#[serde(rename_all = "camelCase")]`, while the other operations use facade
types that do.

The backend client now pins each operation's exact spelling so a future change
fails a test rather than silently degrading. Unifying the spellings would be a
breaking change that requires a TraceDB protocol version bump and coordination
with all clients.

## Verification

Tested against the local trace.db (4,705 sessions, 577,618 events, 1.95 GB):

```sh
cd /Users/hsw/Workspace/trace-db
printf '{"op":"stats"}\n{"op":"list","limit":1}\n{"op":"show","id":"claude:82ebf6c9-471b-40db-95d0-138c360feede","from":0,"to":0}\n' \
  | ./target/release/trace-db api
```

All tests pass:

```sh
cd /Users/hsw/Workspace/sylph-web/backend
cargo test --lib tracedb  # 6 passed
cargo test --test integration import  # 10 passed
```

## What Was NOT Done (Out of Scope)

- **`list` operation:** The current TraceDB release includes it, but exposing it
  through the backend API was not requested and is not necessary for the cleanup.
  The search endpoint already covers browsing the archive.
- **Delete `PHASE5_HANDOFF.md`:** Already removed in prior commits
  (`d98b14b`).
- **Native→Sylph conversion endpoint:** Still an open design decision, unchanged.
