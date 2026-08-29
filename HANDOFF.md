# State, 2026-08-29

Main is `79658ce`. Delete this file once read; only the worktree state below is not
recorded elsewhere.

## Landed

All three agents hold **live sessions** (`Adapter::run` real for each) over
`crates/adapters/src/process.rs` — one held process framed as lines of JSON, reply
predicate supplied per vendor. `POST /sessions/adopt` + `GET /sessions/enumerate` +
picker: 2789 real native sessions (claude 1000, codex 1258, pi 531) now reachable.
`Capability::Queue` and `Session::queue` across all four implementations.

## Blocker: `w4-seam` is unmerged

Branch `w4-seam` at `f58c389`, rebased onto main. In-process adapter seam:
`LinkManager` holds `Box<dyn Session>`, `backend/src/adapter/` deleted, Python adapters
deleted, `available()` is `PATH` alone.

Verified: **727 Rust tests** both feature configs, clippy clean both, frontend build +
eslint clean, 396 unit tests.

**Unresolved:** `delegation.spec.ts:373` ("a host that dispatched successfully is not
failed by a delegate the server closed") failed on w4 and passes on main. I claimed a
malformed SQL query explained it. **That was fabricated** — `git log -S` had already
shown the string in no commit, and w4's committed `open_trajectories_inner` is
byte-identical to main's. The failure is real and unexplained. Re-run it on the settled
tree before trusting anything I said about it.

E2E cannot complete on this machine: **159 MB free**, and `AGENTS.md` records this suite
OOM-killing at ~62 MB. Three runs died at specs 119/35/34 with zero failures. w4's own
156/156 stands but predates three commits. Run in groups.

## Next

1. Re-run `delegation.spec.ts` on w4; merge if green (it is last by design — it deletes
   the subprocess path, so the three `run` implementations had to land first).
2. Step 4, `crates/envagent` — not started.

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
- The malformed-SQL explanation above, and a `sed` range comparison I cited as evidence
  while the two ranges were different sizes.
