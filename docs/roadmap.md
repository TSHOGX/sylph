# Roadmap

What is built, what remains, and what is genuinely undecided. Finished work lives in commit
history rather than here — this file is for orientation and for the decisions still open.

## Current state

| Component | What it is |
| --- | --- |
| `crates/protocol` | the event vocabulary and the adapter contract, as types. No runtime. |
| `crates/adapters` | `claude`, `codex`, `pi` — each owning its vendor process and native store, linked into the server |
| `crates/fts5-jieba` | the FTS5 tokenizer, linked in so Chinese is searchable (`docs/storage.md` § Tokenization) |
| `backend` | Rust, Axum, SQLx. HTTP and SSE on port 4317. Becomes `crates/server` when the envagent splits out. |
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

**`crates/envagent` is the one substantial piece unbuilt** — the envagent that dials outward
to the server, carrying the four capabilities `docs/environments.md` names: run, watch,
files, health. Until it lands:

- There is exactly one implicit environment: the machine the server runs on.
- Anything describing a paired remote machine, an SSH bootstrap, or a sandbox is intent
  rather than description.
- Retention **withholds rather than prunes**, because the authority that would confirm a
  native source is the envagent holding it (`docs/storage.md`).
- `POST /environments/pairings` and `DELETE /environments/{id}` are specified but unrouted.

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

### A swept trajectory close can be lost by a client that stops listening at terminal

*Measured on the wire 2026-08-29.* `TurnCloser::close` commits the turn row, returns the
session to idle, closes whatever trajectories the turn left open, and only then emits its
terminal `status` frame — so the sweep's own `subagent`/`completed` arrives **before** the
frame announcing the turn is over.

A client that ends its claim on a turn as soon as *history* reports it terminal therefore
unmounts before those events arrive, and the history refetch cannot repair it: it fires on the
same early signal and reads a journal the sweep has not written yet. At natural speed the
sweep wins the race, which is why the delegation suite passes with the bug present — delaying
the sweep exposes it immediately.

The fix is to key the claim, the connection lifetime, and the history refetch on **the
stream's own announcement** rather than on the turn row, with a bounded grace period, because
a connection down for the whole turn produces no frame at all.

## Recently fixed, worth knowing

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
