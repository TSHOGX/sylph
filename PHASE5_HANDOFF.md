# Phase 5 Handoff

All five roadmap phases are complete. This records what is not in the code:
decisions that belong to a human, work deliberately deferred, and the traps
that cost time to find.

`docs/roadmap.md` has the per-deliverable status and how each exit criterion
was verified. `docs/installation.md` is the user-facing install guide. This
file is for whoever picks the project up next.

## Open decisions (yours, not mine)

Both carried forward unchanged from `PHASE4_HANDOFF.md`. Neither blocks a
release; both need a human.

**1. Rebuild trace-db? (irreversible — back up first)**

The installed `trace-db` binary and the current source have diverged. The
installed `show` returns a bare event array; the current source returns
`{session: {cwd, title, started_at_ms, ...}, mode, events}`. The installed
build also serves 4 operations where the source declares 5, and spells its
wire format in camelCase where the source uses snake_case.

Sylph Web reads both spellings — every multi-word field carries a serde
`alias` — so it works against either build. But the archive-metadata code
path is inert against the installed binary, because that binary never sends
the metadata it wants.

Rebuilding would light that path up. Before doing it: confirm `api --db`
still works after trace-db's config refactor, and back up `trace.db` first —
4702 sessions and 526995 events, and the migration behaviour of the newer
`store.rs` FTS/tokenizer code against an existing database is unverified.

**2. Native to Sylph conversion: which endpoint shape?**

`PATCH /api/v1/sessions/{id}` or `POST /api/v1/sessions/{id}/convert`. I
recommend the dedicated verb. Converting a native session is the only
legitimate crossing of the no-identity-injection line, so it wants to be an
explicit, auditable operation rather than a field on a general update. A
`PATCH` that happens to flip `identity_mode` makes the most consequential
action in the product look like a rename.

Not implemented either way — no endpoint currently converts a session.

## Deferred, recorded so it stays a choice

- **Virtual scrolling.** First-load JS is 190.4 KB gzipped against a 200 KB
  budget. A windowing library would eat most of the remaining 9.6 KB, so
  `event-list.tsx` memoizes on event identity instead. Revisit only with a
  measured problem at 1000+ events, and expect to renegotiate the budget.
- **Memory-leak checks.** Needs a sustained-load harness that does not exist.
  The SSE projection ring is bounded at 512 events per turn, which is the
  structural defence; nothing has been measured over hours.
- **LCP.** Never measured. First-load JS was, and is the number the roadmap
  cites — do not read it as a proxy for perceived load time.
- **Reboot verification.** `RunAtLoad` is set and `launchctl bootstrap`
  works, but no actual reboot has been tested.

## Traps worth knowing before you change the build

**Next compiles rewrites into the route manifest at build time.** The proxy
target comes from `SYLPH_WEB_BACKEND`, and setting it only when starting the
server does nothing — the built app keeps proxying to whatever was baked in.
This cost real time twice: once as four failing E2E tests that were silently
talking to the developer's own backend, and once as a wrong README
instruction. `scripts/install.sh` and `playwright.config.ts` both set it for
the build; `npm run dev` reads it at startup and needs no rebuild.

**`output: "standalone"` omits `.next/static` and `public`.** The bundle is
otherwise self-contained. Without those two copied in, the server starts
cleanly and every asset 404s, which looks like a broken app rather than a
broken install. The installer copies them and fails the install if `static`
is missing.

**launchd holds the log descriptors.** It opens `StandardOutPath` and
`StandardErrorPath` before exec'ing `start.sh`, so rotating by rename sends
the current run's output into the rotated file. `start.sh` copies and then
truncates in place, which keeps the inode.

**launchd starts from a bare environment.** It provides `HOME`, which the
backend's defaults are relative to, but nothing else. Any override has to be
written into the plist, which the installer does for the variables that were
set when it ran. A moved Node install therefore needs a re-run.

**macOS ships bash 3.2.** `wait -n` is not available; `start.sh` polls both
PIDs with `kill -0` instead.

## Standing constraints

These are product invariants, not preferences. `AGENTS.md` is authoritative.

- Session origin is durable data. Never inject `SOUL.md`, reset, compact, or
  rewrite prompts for a `native` session unless the user explicitly converts
  it. This is why decision 2 above wants an auditable verb.
- The runner protocol is an execution boundary. Do not reimplement it in the
  web app.
- Never query TraceDB's SQLite directly. Go through its JSON API; lineage
  folding and relevance ranking belong to TraceDB.
- Loopback only. Any non-loopback deployment is a separate authentication and
  threat-model change, not a config flag.
- Migrations are now mandatory. Before Phase 5 the database was disposable;
  a packaged install means someone's journal is real. `--rollback` restores
  the journal alongside the binary precisely because an older binary refuses
  a schema that is ahead of it.

## One real bug this phase's testing found

`RunnerEvent` declared `#[serde(alias = "is_error")]` on a field already
named `is_error`, so the alias was a no-op and the protocol's actual spelling
— `isError`, which three of four shipped runners emit — never parsed. Every
tool failure deserialized as `None`, became `false`, and was journalled as a
success. No parse error, no warning.

It surfaced only because a test asserted both spellings of the wire contract
rather than the implementation's view of it. Worth remembering when deciding
what a test is for.

