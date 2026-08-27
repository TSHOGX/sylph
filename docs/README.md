# Sylph Documentation

Sylph is a coding-agent workspace. A **server** holds the authoritative record of every
session and owns all session policy; an **envagent** runs on each machine where agents
execute and dials back to the server; a **web frontend** is one transport onto the
server. `architecture.md` is where to start.

## Read in this order

1. **[architecture.md](architecture.md)** — the system's shape, the session model,
   identity, authority, AFK, and the security gate. Everything else defers to it.
2. **[environments.md](environments.md)** — the envagent wire protocol, spooling,
   pairing, and how SSH and sandbox environments come to exist.
3. **[session-protocol.md](session-protocol.md)** — the adapter contract: one event
   vocabulary, three modes, capability facets, requests, delegated trajectories.
4. **[agents.md](agents.md)** — per-vendor knowledge for `claude`, `codex`, `pi`.

Then, by subject:

| Question | Authoritative source |
| --- | --- |
| What is built, what is next | [roadmap.md](roadmap.md) |
| System design, session model, identity, authority | [architecture.md](architecture.md) |
| Where a session's work runs, and the wire to it | [environments.md](environments.md) |
| What an adapter may send, and when | [session-protocol.md](session-protocol.md) |
| A vendor CLI's shapes, quirks, and control surface | [agents.md](agents.md) |
| Endpoint contracts | [api-design.md](api-design.md), [api-v1.openapi.json](api-v1.openapi.json) |
| Tables, indexes, migrations | [database-schema.md](database-schema.md) |
| How history is kept, replayed, pruned | [storage.md](storage.md) |
| How the frontend is built | [frontend-architecture.md](frontend-architecture.md) |
| Panes, surfaces, components, tokens | [ui-layout.md](ui-layout.md) |
| Where files live, what a setting does | [configuration.md](configuration.md) |
| How to install and deploy | [installation.md](installation.md) |
| How to develop locally | [development.md](development.md) |

## What changed in this rewrite, and why the file list is shorter

Four documents used to argue the authority question — `architecture.md`,
`data-architecture.md`, `control-plane.md`, and `environments.md` — which is how they
came to disagree with each other. `data-architecture.md` and `control-plane.md` are
**deleted**, absorbed into `architecture.md`. A claim now has one home.

Five design changes run through all of these, each with its own section in
`architecture.md`:

- **One class of session.** `origin`, `identity_mode`, and `lifecycle_mode` are gone.
  Measured, they had four write sites and one behavioural branch between them, so a spec
  had hardened a distinction the implementation never made. What survives is
  `inject_identity`, a boolean with a default.
- **Identity is `(environment, agent, native_id)`.** No server-minted UUID, so no second
  id space and no comparison that is silently always false.
- **One environment abstraction.** Not a trait with an implementation per substrate; one
  envagent that dials in, differing only in how it came to exist.
- **One store, one vocabulary.** Native history is parsed by an adapter into the same
  events a live turn emits. The separate archive, its client, and its id translation are
  gone — and with them the open question about search covering only part of history.
- **Three agents.** `claude`, `codex`, `pi`. `gemini` and `opencode` are unsupported,
  which closes two unresolved questions that were gating design work.

## Principles

**One authority.** The server's store is the record. Everything else is a source it
ingests from or a projection it serves. `AGENTS.md` § Compatibility governs the
consequence: no other consumer exists, so schemas and wire formats change in place, and
compatibility code is forbidden rather than merely discouraged.

**Local-first is about dependencies, not geography.** A server on a remote host is still
local-first, because the authority is its own store and not somebody else's API.

**Agent workspace, not chat app.** Tool execution is primary content; assistant prose is
connective tissue; thinking is debugging detail.

**Fidelity must be visible.** A record says how much resolution its source had, because
a waterfall drawn over message-granularity data presents equal-length bars as
measurements.

**Accessibility is a requirement.** WCAG 2.1 AA, verified by automated testing and by
manual screen-reader passes — and those catch different things. Well-formed ARIA that a
real reader still fails has happened three times here: axe checks that the graph is
valid, a screen reader walks it.

## Where a decision lives

**Rationale goes in a code comment next to what it governs**, not in a separate decision
log. `frontend/components/timeline.tsx` is the intended density: it explains why
memoization was chosen over virtualization and what evidence would reverse that call.

**A durable gotcha goes in `AGENTS.md` § Pitfalls.** A cross-cutting design argument goes
in the governing document above. Progress goes in commit messages and
[roadmap.md](roadmap.md) — do not add progress, handoff, or summary documents, because
they duplicate the roadmap and then drift from it.

**A measurement is only quotable while the thing it measured still exists**, and a number
repeated in four places is four chances to drift. Cite it once and point at it — this
file's own history has an instance where one population was recorded as two different
figures with two different agents, and nothing could catch it.

## When code and spec disagree

Neither wins automatically. Identify whether behaviour or intent should move; if the
design changed, update the governing document first, then the code; if evidence
invalidated the design, update the document and say what the evidence was.

Do not silently rewrite a spec to bless an implementation accident. Do not contort code
around a design evidence has invalidated. Both failures have shipped here, and the second
is what this rewrite corrects.

## Design documents

Two files are pre-implementation design arguments rather than descriptions of the code:
[agent-ui-design-analysis.md](agent-ui-design-analysis.md) and
[keyboard-shortcuts-default-sessions-settings.md](keyboard-shortcuts-default-sessions-settings.md).
They are kept because the reasoning still governs the UI, and each opens with a banner
saying what kind of document it is. Where they disagree with
`frontend-architecture.md`, `ui-layout.md`, or `roadmap.md`, those win.

## Language

Code, documentation, and commit messages are English only. Conversation happens in
whatever language the user writes in; durable artifacts stay English regardless.
