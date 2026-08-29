# State, 2026-08-29

Main is `79658ce`. Delete this file once read; only the worktree state below is not
recorded elsewhere.

## Landed

All three agents hold **live sessions** (`Adapter::run` real for each) over
`crates/adapters/src/process.rs` — one held process framed as lines of JSON, reply
predicate supplied per vendor. `POST /sessions/adopt` + `GET /sessions/enumerate` +
picker: 2789 real native sessions (claude 1000, codex 1258, pi 531) now reachable.
`Capability::Queue` and `Session::queue` across all four implementations.

## All five worktrees are merged

`w4-seam` landed as `df1aed6`. **The regression I reported on it did not exist.**
`delegation.spec.ts:137` fails on `main` identically — same locator, same
`"Strandedrunningfocus"` — and passes 22/22 on w4's tree. w4-seam established this and was
right; I had it backwards, twice: first inventing a malformed-SQL cause, then treating a
main-branch race as a branch regression.

The real cause is now in `AGENTS.md`: `Stop` disappearing means the turn is *terminal*, and
the settle sweep runs ~35 lines later. Every spec asserting on swept state races it, and
`:137`, `:373`, and `:348` have each been the failing one on different runs of the same tree.

## Next

1. **Step 4, `crates/envagent`** — not started. The whole of what remains.
2. Make the sweep-racing specs wait on something the sweep produces rather than on `Stop`.
   Small, and it retires a failure that reads as a regression on whatever branch is under
   test.

## Verify subagent reports

Six reports this session claimed green against broken trees: one didn't compile, one had
a failing test, one reported a hash that did not exist with 48 files uncommitted, one
claimed two code changes that were not in the file. The rule that catches it: not "run
the suite" but **run it after the last edit**, and paste what it printed.

## My own corrections, so they are not re-inherited

- `pi` queues with `follow_up`, **not** `queueUserMessage` (occurs zero times in that tree).
- `pi`'s `agent_start`/`agent_end` **do** emit and reach the wire; the "zero emit sites"
  claim came from reading a minified bundle.
- The steer/queue question needed **no** token probe; the stdin-iterator shape settles it.
- The malformed-SQL explanation, and a `sed` range comparison I cited as evidence while the
  two ranges were different sizes.
- Reporting `delegation.spec.ts:137` as a branch regression when it fails on `main` too. I
  had run it on main *alone* with `-g`, where it passes; the whole file is what reproduces
  it. Running one spec in isolation is a different measurement from running its file.
