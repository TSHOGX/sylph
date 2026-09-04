# Roadmap

What is built, what remains, and what is genuinely undecided. Finished work lives in commit
history rather than here — this file is for orientation and for the decisions still open.

## Current state

| Component | What it is |
| --- | --- |
| `crates/protocol` | the event vocabulary and the adapter contract, as types. No runtime. |
| `crates/adapters` | `claude`, `codex`, `pi` — each owning its vendor process and native store, linked into the server |
| `crates/fts5-jieba` | the FTS5 tokenizer, linked in so Chinese is searchable (`docs/storage.md` § Tokenization) |
| `backend` | Rust, Axum, SQLx. HTTP/SSE on port 4317 plus the opt-in envagent ingress bridge. |
| `crates/envagent` | Rust envagent with v1 framing, WAL spool, reconnect epochs, local path policy, credentials, and adapter hosting. |
| `frontend` | Next.js App Router, React, TanStack Query |
| store | SQLite: `journal.db` hot, `history.db` warm with the full-text index, `blobs/` beside them |

**What the system does.** Sessions run against the three agents and stream tool and
reasoning events as they happen. A session's identity is `(environment, agent, native_id)`,
so a vendor's own session can be adopted — parsed from its native store into the same events
a live turn emits — with no second id space and no separate archive. Delegated work renders
as a navigable tree defined by *use* rather than declaration, so a vendor's new delegating
tool needs no whitelist entry. Turns are server-side resources: closing a tab mid-turn and
reopening resumes the stream. History is lossless and tiered, with replay and full-text
search over everything the server holds. Capability surfaces are facet-scoped, and every
control is typed rather than prose sent as a prompt.

**Measure rather than quote.** Test totals, bundle sizes, and session counts are
deliberately not recorded here — they go stale silently and nothing checks them
(`docs/README.md` § How to write these documents):

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace                        # Rust unit + integration
cd frontend && npm test                       # frontend unit
cd frontend && npx playwright test --list     # E2E spec count
./scripts/measure-bundle.sh --route='/'       # first-load JS, per route
```

**The first-load budget is 200 KB gzipped for `/`,** and it exists to protect first paint on
a phone over a slow link rather than as a score to keep down. Current figures are past it,
knowingly: the budget is the number that triggers a look, not one that vetoes a feature.
Quote a route alongside every figure — the session route legitimately carries the timeline,
the delegation tree, and the overview, so comparing it against a budget set for `/` has
already caused confusion twice.

## What remains

**The envagent/runtime protocol foundation is implemented, but the product cutover is not.**
The binary dials outward, hosts adapters, applies local path policy, and keeps an ack-gated
WAL spool; the server has pairing, leases, durable producer receipts, session edges, and an
opt-in framed ingress listener. The remaining work is the integration layer the platform
closure gate requires:

- Route session creation, turns, watch/adopt enumeration, files, health, and control through
  `EnvBridge::dispatch`; today those API paths still use the in-process local adapter seam and
  reject a paired environment as unreachable.
- Carry native turn-input facts, source cursors, and child-session ownership from the adapter
  stream into durable server records. The wire types exist, but the adapter message stream does
  not yet produce those facts and Codex child events still arrive through the root stream.
- Replace the default local execution path with a real envagent connection and add the
  multi-process commit/ack/reconnect fault-injection and browser gates. `SYLPH_ENV_LISTEN`
  currently enables an opt-in development ingress rather than that cutover.
- Retention **withholds rather than prunes** when the paired native source cannot confirm a
  bound (`docs/storage.md`).

**A sandbox Template carrying the agent CLIs** is needed before the Khora path can be tested
end to end, including whether a sandbox session's retention class resolves as the policy
expects. None exists yet.

## Open decisions

### Whether *reading* should be path-bounded, as acting already is

Acting is bounded: a client-named `workdir` must canonicalize inside `browse_roots` plus the
Quick Start directory, on both `POST /sessions` and `POST /sessions/adopt`, and `/fs/*` is
bounded by the same set through the same primitive — one root set, so the picker cannot offer
a directory creation would refuse.

Reading is not. `GET /sessions?workdir=` takes any absolute path, `/search` has no path
filter and reaches every transcript the store holds, and `/workspaces` lists every directory
any session ever ran in. So a client can learn what ran where on this machine.

The line that is currently true and coherent: **reading a record is authentication-bounded,
acting on one is browse-root-bounded.** Under the loopback bind and the bootstrap cookie that
is defensible, and it is the whole answer only for as long as both hold. This is a
threat-model call rather than an implementation gap, and it has to be answered for all three
surfaces at once or not at all — bounding one reads as a boundary without being one. Any move
off loopback forces it, and so does the envagent: a paired machine means "this server can
read it" stops implying "the operator sitting at it can".

### Multi-user is un-foreclosed, not solved

Authorization has one place to live and request resolution records a resolver, but
authentication beyond the bootstrap cookie, per-user visibility, and credential isolation are
unbuilt. **Who owns an envagent** is the question this architecture adds, and without an
answer one user can run agents on another user's laptop. Non-loopback binding stays a
separate, explicit change (`docs/architecture.md` § Security model).

### A terminal session's growth is reported and deliberately unbounded

`/diagnostics` names which sessions hold what and how fast each is growing. A configurable
size cap is **refused rather than deferred**: a terminal session's record is the only copy, so
a size-triggered prune is a size-triggered deletion of data nothing else holds. The remedy has
an owner — delete the session, or pair the environment holding its native source so it stops
being terminal.

## Known bugs

No separately tracked product defect is outstanding. The incomplete platform integration
listed under **What remains** is implementation work and blocks the platform milestone; it
must not be read as a completed local cutover.

The former workflow zero-duration defect is closed by the explicit time axes below: an absent
vendor close remains absent rather than becoming a fabricated zero.

## Recently fixed, worth knowing

**Trajectory interior reachability is explicit.** `InteriorState` distinguishes `pending`,
`partial`, `full`, `forwards_nothing`, and `unreadable` across the protocol, journal, history,
SSE, and frontend. Producers author transitions from source facts; consumers no longer infer
meaning from an empty child list. A shared monotone merge keeps a late `pending` notification
from erasing a native source already proven readable.

**Event time has three independent axes.** `seq` is the only replay/order authority;
`arrivalAtMs` is the server receipt clock; `recordedAtMs` is the vendor clock when present.
`createdAtMs` remains a compatibility timestamp but is no longer used to mix live and native
records or to infer a duration. The waterfall only subtracts endpoints in one domain.

**Process stdout retains lexical native bytes.** The shared process reader yields a parsed value
with the exact JSON line bytes. Claude stores those bytes in `NativeRecord.raw_json` before
projecting an event, preserving whitespace, key order, duplicate keys, and escape spelling for
future re-projection; Codex and Pi continue to consume only parsed values.

**Claude native ingestion now follows one bundle reader in every mode.** `adopt`, `watch`,
and live stdout attachment share source discovery, cursor advancement, manifest revisions,
sidecar classification, and projection. A resumed live session captures a pre-launch
baseline for every existing source, so historical delegates are hydrated but never
projected again; only later appends are live. Root stdout and native-only completion facts
share one adapter-level correlation state, which is what lets synchronous Agent closes and
embedded task notifications arrive exactly once. Sidecars are retained as validated,
on-demand resources rather than read wholesale on the event path. Raw persistence is
required: adopt fails explicitly and a live link closes with `LINK_LOST` when the source
record cannot be retained, rather than continuing with an unrepeatable projection.

**Which sidecar stream to suppress is decided from the host call's tool name**, not from state
shared with the stdout pump. `--forward-subagent-text` forwards for the built-in `Task` alone,
so the host's name is the vendor's own answer to "is stdout already carrying this interior?",
and it is recorded before the delegate file exists. The rejected design — a set of trajectory
ids the stdout pump wrote and the follower read — asked instead whether stdout had *mentioned*
the delegate yet, which is a question about arrival order rather than about the vendor, and it
required a mutex the follower could not take from its runtime thread. An unknown host is read
rather than suppressed, because a duplicate is merged by every consumer keying on `stream_id`
while a wrong suppression loses an interior with no other source. Native records are retained
losslessly in `history.db` with a dedicated `raw` retention class, and large persisted-output
sidecars are content-addressed for on-demand reads. Embedded `<task-notification>` XML is
projected through the same correlation handler as live notifications, so an async close lands
on its trajectory rather than being mistaken for assistant prose.

**A terminal turn status now means the record is finished.** `TurnCloser::close` used to
commit the turn row terminal and *then* run the settle sweep, so `status` was answering two
questions with one column — what the turn produced, and whether its record was complete —
with no field that separated them. A client ending its claim on the row therefore tore down
the stream before the sweep's own `subagent`/`completed` events arrived, and its history
refetch fired on the same early signal and read a journal the sweep had not written yet: a
delegation the server had closed rendered as running forever.

Settling is now two writes with the sweep between them, through a new non-terminal
`settling` status (`docs/database-schema.md` § A terminal status means the record is
finished). Three things are worth carrying forward:

- **The client-side fix was the wrong half.** Keying the claim and the refetch on the
  stream's own announcement with a bounded grace period was the plan of record here, and it
  would have left the server publishing a state no client could name — so every future
  reader would have re-derived the same timer. A previous attempt at it also cascaded into
  rewriting the page's whole turn-lifecycle model. The server-side fix is one enum member,
  and it deleted the reason the frontend's two workaround comments existed.
- **It presented as flakiness, and that was the expensive part.** One run in three failed on
  a *different* spec each time, because whichever assertion lost the race was the one that
  failed. The E2E suite had already been shaped around it: `runPromotion` was deliberately
  left with the weaker of two available waits, and `sweptClosed` carried a measurement of why
  the client could not deliver the settle signal at all. Both comments named this entry and
  predicted the fix.
- **The Rust suite got stricter for free.** `await_turn` polls exactly the four terminal
  statuses, so it now waits through the sweep rather than returning mid-sweep — every test
  that settles a turn and then asserts on swept state stopped racing without being touched.
  Verified rather than assumed: a throwaway test asserting all 48 of a turn's abandoned
  trajectories are closed the moment `await_turn` returns passes, and the same assertion is
  what `a_terminal_turn_always_has_its_trajectories_closed` makes permanent.

**The OpenAPI turn-status enum had drifted in both directions**, undetected because nothing
compared it to anything: it declared `stopped`, which no code has ever written, and omitted
`starting`, `stopping`, `cancelled`, and `interrupted`, all four of which a client can be
handed. `every_turn_status_the_schema_declares_is_one_the_journal_allows` now reads the
vocabulary out of the migration and compares, rather than holding a third copy of it.

**Segmented CJK search now ships.** `crates/fts5-jieba` is linked into the server and
registered per connection, so Chinese is searchable by word rather than as one opaque token
(`docs/storage.md` § Tokenization). Two things came out of the integration:

- **Static linking is the requirement, not the convenient option.** An index built with one
  tokenizer and queried with another silently returns nothing, so a missing loadable
  extension would answer *wrongly* rather than worse. There is no fallback and a
  registration failure is fatal.
- **The dictionary costs ~600 ms and FTS5 builds a tokenizer per table per connection.** A
  per-instance dictionary multiplied that by the pool size and pushed a consolidation test
  past its budget; sharing one process-wide `OnceLock` fixed it and took the storage suite
  from 4.1 s to 0.8 s.

**A search query containing punctuation returned 500.** FTS5 reads punctuation as syntax, so
`needle-one` parsed as `needle NOT one` and SQLite answered `no such column: one` — an error
naming a column, three layers from the text box that typed it. The query is now quoted into a
phrase (`HistoryStore::search`).

Three things about that one are worth carrying forward. **Binding a value as a parameter was
not sufficient**, and the comment at the call site claimed it was: binding protects against
SQL injection, and the bound value still reaches FTS5's *expression* parser. **Every existing
search test used a single word**, which is the blind spot `docs/storage.md` names — so the
defect was invisible to a green suite while the rule sat documented and unimplemented. And a
test *asserted the bug as intended behaviour*, pinning that a malformed query must error;
when a test encodes a defect, fixing the defect fails the test, which is the moment to ask
which of the two is wrong.

## Suggested, not scheduled

**Make OpenAPI path coverage a test.** A route added to `backend/src/lib.rs` without a
matching path in `docs/api-v1.openapi.json` is the recurring failure, and the check is a short
script diffing `.route(...)` calls against the spec in both directions.

The precedent is `error::tests::every_emitted_code_is_published_in_the_openapi_enum`, and it
comes with a warning worth heeding: that test once missed six emitted codes because the list of
variants *it* enumerated was hand-maintained. **A test whose subject is a hand-maintained
mirror must not itself contain one** — enumerate with a `match` carrying no wildcard arm, so
the compiler refuses a new variant until it is listed. A route-coverage test has to derive both
sides or it reproduces exactly that failure. Read any "every X is Y" assertion for how it
enumerates X.
