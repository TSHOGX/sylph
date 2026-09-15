# Sylph Documentation

Sylph is a coding-agent workspace. A **server** holds the authoritative record of every
session and owns all session policy; an **envagent** runs on each machine where agents
execute and dials back to the server; a **web frontend** is one transport onto the
server.

Start with [architecture.md](architecture.md) for identity and authority, then
[unified-trace-foundation.md](unified-trace-foundation.md) for the trace model. Cross-cutting
decisions are recorded in [ADR-0001](adr/0001-foundation-semantics.md) and the ADRs listed
below; implementation work is ordered by [roadmap.md](roadmap.md).

## Architecture decisions

- [ADR-0001](adr/0001-foundation-semantics.md) — protocol foundation, producer receipts,
  session edges, adopted input, and command ambiguity.
- [ADR-0002](adr/0002-environment-workspace-subject.md) — Subject, Environment, Workspace,
  and environment-level access.
- [ADR-0003](adr/0003-unified-envagent-execution.md) — envagent as the sole product execution
  path, including the local machine.
- [ADR-0004](adr/0004-concurrent-control-ordering.md) — concurrent control requests and
  server ordering.
- [ADR-0005](adr/0005-raw-retention.md) — lossless raw retention and future deletion.
- [ADR-0006](adr/0006-environment-authority-and-capability-grants.md) — environment authority and capability grants.

## Read in this order

1. **[architecture.md](architecture.md)** — the system's shape, the session model,
   identity, authority, AFK, and the security gate.
2. **[environments.md](environments.md)** — the envagent wire protocol, spooling,
   pairing, and how SSH and sandbox environments come to exist.
3. **[unified-trace-foundation.md](unified-trace-foundation.md)** — trace objects,
   ownership, delivery, history/context, measurements and evidence resolution.
4. **[unified-trace-storage.md](unified-trace-storage.md)** — capture, durable receipts,
   reduction, publication, snapshots and bounded query protocol.
5. **[session-protocol.md](session-protocol.md)** and **[agents.md](agents.md)** —
   process control, capability facets and vendor acquisition; trace mappings live in
   the three dedicated adapter specs below.

Then, by subject:

| Question | Authoritative source |
| --- | --- |
| What is built, what is next | [roadmap.md](roadmap.md) |
| System design, session model, identity, authority | [architecture.md](architecture.md) |
| Unified trace meanings, identities, delivery, runtime work and history/context | [unified-trace-foundation.md](unified-trace-foundation.md) |
| Trace evidence storage, atomic publication and bounded read API | [unified-trace-storage.md](unified-trace-storage.md) |
| Codex trace capture and semantic mapping | [unified-trace-codex.md](unified-trace-codex.md) |
| Claude Code trace capture and semantic mapping | [unified-trace-claude.md](unified-trace-claude.md) |
| Pi trace capture and semantic mapping | [unified-trace-pi.md](unified-trace-pi.md) |
| Trace reader, navigation, interaction and accessibility | [unified-trace-ui.md](unified-trace-ui.md) |
| Trace evidence corpus and falsifiable implementation acceptance | [unified-trace-conformance.md](unified-trace-conformance.md) |
| Where a session's work runs, and the wire to it | [environments.md](environments.md) |
| What an adapter may send, and when | [session-protocol.md](session-protocol.md) |
| A vendor CLI's shapes, quirks, and control surface | [agents.md](agents.md) |
| Endpoint contracts | [api/](api/README.md) |
| Tables, indexes, ancestry | [database-schema.md](database-schema.md) |
| Storage routing and retained source-comment anchors | [storage.md](storage.md) |
| How the frontend is built | [frontend-architecture.md](frontend-architecture.md) |
| Panes, surfaces, components, tokens | [ui-layout.md](ui-layout.md) |
| Where files live, what a setting does | [configuration.md](configuration.md) |
| How to install and deploy | [installation.md](installation.md) |
| How to develop locally | [development.md](development.md) |
| Why a vendor claim is believed | [measurements/](measurements/codex/README.md) |
| A decision's context and rejected alternatives | [adr/](adr/) |
| Whether every pointer in the tree still resolves | `cargo run -p sylph-docs-check` |

## Principles

**One authority.** The server's store is the record. Everything else is a source it
ingests from or a projection it serves.

**Local-first is about dependencies, not geography.** A server on a remote host is
still local-first, because the authority is its own store and not somebody else's API.

**Readable work.** Inputs and answers orient the conversation; progressively disclosed
execution preserves every detail without overwhelming the reading flow.

**Fidelity must be visible.** A record says how much resolution its source had,
because a waterfall drawn over message-granularity data presents equal-length bars as
measurements.

**Accessibility is a requirement.** WCAG 2.1 AA, verified by automated testing *and*
by manual screen-reader passes, because they catch different things: an automated
checker verifies the ARIA graph is valid, a screen reader walks it. Well-formed ARIA
that a real reader still fails is a defect.

**Performance is a commitment, not a number.** Where a budget appears it exists to
protect an experience — the first-load budget keeps the app usable on a phone over a
slow link — so the budget and its reason belong in the docs while the current
measurement belongs to the script that takes it.

## How to write these documents

**State rules in the present tense.** A specification says what is true, not how it
came to be true. A rule that was learned from a defect is stated as the rule; the
incident belongs in the commit that fixed it.

**One claim, one home.** A rule stated in two places is a rule that will diverge, and
nothing checks the two against each other. Cite it once and point at it from
elsewhere.

**Prefer a shape to a count.** "This key appears only on a `user`-role block" stays
true; "on 3226 files" goes stale the moment the store grows. Where a magnitude
genuinely drives a decision, give the magnitude *and* the command that re-derives it,
and never carry a measurement forward as current.

**A measurement is quotable only while what it measured still exists.** A figure
taken against a deleted component cannot be re-derived and cannot be checked, so it
is not evidence any more.

**Rationale goes next to what it governs.** A design principle belongs in the
governing document here; the mechanism that implements it belongs in a comment beside
the code. Neither should restate the other at length.

**Progress belongs in commits and [roadmap.md](roadmap.md).** Do not add progress,
handoff, or summary documents: they duplicate the roadmap and then drift from it.

## When code and spec disagree

Neither wins automatically. Identify whether behaviour or intent should move; if the
design changed, update the governing document first, then the code; if evidence
invalidated the design, update the document and say what the evidence was.

Do not silently rewrite a spec to bless an implementation accident. Do not contort
code around a design evidence has invalidated.

## Language

Code, documentation, and commit messages are English only. Conversation happens in
whatever language the user writes in; durable artifacts stay English regardless.
