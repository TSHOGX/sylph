# ADR 0006: Environment authority and capability grants

- Status: Accepted
- Date: 2026-09-07
- Relation: refines [ADR-0002](0002-environment-workspace-subject.md) decisions 5 and 6. Session
  identity is unchanged.

## Context

[ADR-0002](0002-environment-workspace-subject.md) settled what a Session, Workspace, and Subject
are, and made authorization environment-level. It left one question open, and `docs/environments.md`
named it as the gate on multi-user: who may authorize a Subject on a given environment.

The question is sharper than access to records. A grant that admits control admits creating a
session, a session names a `workdir`, and that `workdir` is the `current_dir` of a spawned agent.
Within `browse_roots` — which always includes `$HOME` — that is arbitrary code execution on someone
else's machine. An authorization model whose smallest non-empty grant is a shell is not an
authorization model.

Two further facts shape the answer. A machine's authority over itself is physical: the OS-user who
can already run commands there is the only party whose permission means anything. And a
server-provisioned sandbox has no such party, so its authority has to come from the side that
created it.

## Decisions

### 1. An environment has one authority root, fixed at creation

| `grant_authority` | Who may authorize a Subject | Applies to |
| --- | --- | --- |
| `machine` | the OS-user of that machine, through a local command | self-installed, SSH-bootstrapped |
| `server` | a Subject flagged `is_admin` (`docs/api/admin.md` § The server administration capability) | Khora sandboxes |

The value is derived from pairing provenance and is immutable thereafter, in the same way and for
the same reason as `inject_identity`: it describes where the environment came from, and a record
that can be re-pointed later describes nothing. Conversion between roots is refused in both
directions until a concrete requirement defines one.

Delegation is a mechanism rather than a third root. A Subject holding a grant received one; it did
not acquire authority.

### 2. Three capabilities, split where the risk changes

| Capability | Grantable | Covers |
| --- | --- | --- |
| `observe` | yes | reading sessions, transcripts, raw records, search, health |
| `control` | yes | prompt, stop, approval, writes inside a session's `workdir`, creating a session within `browse_roots` |
| `administer` | **no** | `browse_roots`, agent configuration, issuing and revoking grants, unpairing |

`observe` and `control` are the facet pair the link model already uses
(`docs/architecture.md` § One link, two typed capability facets), so authorization reuses the
vocabulary rendering already speaks rather than introducing a parallel one. The reuse is of the
*names* only: a facet reports whether Sylph can reach an environment and a grant reports what a
Subject may ask of it, so the two are separate fields with separate cache rules (decision 7).

`administer` is the authority root itself, named. Holding it is a property of being the machine's
OS-user or a Subject flagged `is_admin`; it is never the subject of a grant. This makes the
non-delegation rule below a consequence of the capability set rather than a separate prohibition,
and it keeps the one operation that *widens* the execution boundary — editing `browse_roots` —
outside anything a grant can convey.

Ungrantable does not mean unreachable, and the two must not be conflated. `administer` covers
operations whose state lives in two different places, so its carrier splits accordingly:
`browse_roots` and agent configuration are envagent-local and have no route at all — only the
machine holding a path can canonicalize it — while grant revocation and unpairing change server
tables and are therefore server routes, gated by the authority root rather than by a grant.
`docs/api/admin.md` § `administer` is ungrantable is authoritative for the split and for each
gate.

### 3. Granting requires the authority root, so a grant cannot be re-granted

A Subject holding `observe` or `control` cannot authorize another Subject through Sylph. Every new
grant returns to the environment's authority root: a local command on a `machine` environment, an
`is_admin` Subject on a `server` one.

Scoped precisely: this bounds Sylph's own grant mechanism. An OS account, an SSH credential, or a
vendor credential on the target machine confers whatever it confers, and SSH-bootstrapping is a
designed transitive path (`docs/architecture.md` § Topology). The claim is that Sylph adds no
escalation path of its own, not that the machine has none.

### 4. Issuing an invite is a local operation, outside the wire

`sylph-envagent invite` runs on the machine and requires that machine's OS-user. `sylph.env.v1`
carries no message that issues a grant.

The distinction is between a machine and a process running on it. If issuance were a wire message,
a compromised envagent could admit new Subjects to itself, which converts one compromised process
into a durable multi-party foothold. Keeping issuance out-of-band means an attacker must already
hold what the grant would give.

### 5. Pairing and invite are separate flows over one code shape

| Flow | Effect |
| --- | --- |
| `pair` | mints an environment id, fixes `grant_authority`, stores credentials, grants the pairing Subject |
| `invite` | adds one Subject grant to an existing environment |

Both use a short-lived single-use code, which is the shape already proven by pairing
(`docs/environments.md` § Pairing). They stay separate endpoints and separate server paths because
an invite that reached the pairing path would mint a second environment id for a machine that
already has one — and the id is a component of session identity, so that is identity corruption
presenting as a duplicate row in a list.

### 6. The server validates five properties of a redeemed code

Environment scope, capability, expiry, single consumption, and issuer provenance. The grant table
lives in the server, so every check is the server's (`docs/architecture.md` § One authority). Scope
is the load-bearing one: a grant asserts only over the environment that issued it, or a compromised
envagent could admit Subjects to environments it does not own.

### 7. Revocation reaches work already in flight, through a field of its own

Revoking a grant takes effect in three stages: new control is refused; the caller's
`authorization` reports `not_granted`; and already-dispatched work retains the control
revocation/interruption request separately from its observed vendor outcome. A request
to stop is not proof of termination. Execution reason and observation availability remain
distinct under `docs/unified-trace-foundation.md` § Independent lifecycle claims.

**The middle stage is not the `control` facet, and that correction is load-bearing.** The
first draft of this decision moved the facet to unavailable, reusing the vocabulary
rendering already speaks. But a facet answers "can Sylph drive this" — a property of the
environment and the vendor, identical for every observer — while a grant is per-Subject.
One field answering both would give one session two link states depending on who asked
(`docs/architecture.md` § A facet describes the environment, never the caller), and the
damage would have surfaced in the cache rather than in the authorization code: the two
unretryable facet reasons are cacheable like a success, so a withdrawn grant would have
been a denial outliving the grant, and a newly issued one would not have applied until the
tab reloaded.

So authorization is a sibling field, never cached, and `controlBlock` is the derived
answer a composer acts on. Revocation is then legible in two places that mean different
things: the link says this caller holds no grant *now* — revocation deletes the row, so
"revoked" and "never granted" are indistinguishable there and one value covers both — and
the turn carries `grant_revoked` as the permanent account of what stopped the work.

`abandoned` is the existing vocabulary for "nobody reported an outcome", is distinct from `failed`,
and is non-final, so a late result is still recorded against the turn that owns the trajectory
(`docs/session-protocol.md` § Outcomes). A revocation-specific status would be a second name for a
state the store already models, and the two would diverge as soon as either grew a case.
`grant_revoked` is a value of the one `CloseReason` vocabulary rather than a reason this
decision invents, which is what keeps it comparable with `spool_full` — two conditions that
cut a turn short the same way.

The sweep is scoped to work whose authorization originated from the revoked grant, by the
`origin_subject` [ADR-0004](0004-concurrent-control-ordering.md) decides the record retains;
it does not terminate unrelated turns on the same environment. The store persists that field
and closes affected work with `CloseReason::grant_revoked`, so an unscoped sweep cannot quietly
kill every turn on the environment or leave revoked work running.

### 8. Disclosure happens twice, with different content

Pairing states that the machine's sessions, transcripts, and native history will reach the server.
An invite states the concrete blast radius of the grant being issued: the resolved `browse_roots`,
the agent CLIs present, the raw transcript exposure, and the capability conveyed.

Pairing is a user configuring their own machine; an invite hands that reach to someone else. Only
the second needs resolved values, and generic wording there would describe every environment
identically while the exposures differ per machine.

## Alternatives rejected

- **One `control` capability covering configuration.** Editing `browse_roots` widens where agents
  may run, so bundling it with sending a prompt lets the narrower grant enlarge itself — the same
  defect as a single capability, one level down.
- **A grantable `administer`.** Restores the escalation chain that decision 3 removes, and makes
  the authority root reachable by a path that never touches the machine.
- **Issuing grants over `sylph.env.v1`.** Convenient, and it makes a compromised envagent
  self-extending.
- **One combined pair/invite flow.** Mints a second environment id for an already-paired machine
  and corrupts session identity.
- **A distinct `uncertain` status for revoked in-flight work.** Duplicates `abandoned` and forces
  every renderer to handle two spellings of one fact.
- **Reporting revocation through the `control` facet.** This decision's own first draft, rejected
  on review. It reuses a vocabulary rendering already speaks, which is what made it attractive, and
  it converts a per-environment fact into a per-caller one — so one session acquires two link states
  and the facet cache policy starts applying to authorization. The alternative *within* that
  alternative — declaring the facets a per-caller projection and rewriting the cache section — was
  also rejected: it makes every facet uncacheable to accommodate one field that never was, and
  removes the property that lets two clients agree about a machine.
- **Leaving the composer to combine authorization and reachability itself.** Two fields a client
  must fold is two chances to fold them differently, and the disclosure ordering — authorization
  before environment state — would be re-decided in every renderer. `controlBlock` is derived once
  on the server.
- **Session owners or directory ACLs now.** [ADR-0002](0002-environment-workspace-subject.md)
  defers both, correctly: a machine owner who wants a narrower surface already narrows
  `browse_roots`, which is a per-environment execution boundary that exists today.

## Consequences

Every read and control surface filters by capability rather than by environment membership alone.
The schema carries `environments.grant_authority`, `subjects.is_admin`, and a grant table with
subject, capability, provenance, expiry, and consumption state.

Two consequences are not about grants and are easy to miss. The wire grows an `authorization` field
and a derived `controlBlock` beside the link facets, with a cache rule of its own — never — because
the facets' rule is sound only for facts about a machine (decision 7). And the two administrative
operations whose state lives in the server, revocation and unpairing, are HTTP routes gated by the
authority root; `browse_roots` and agent configuration remain envagent-local with no route at all.
Ungrantable and unreachable are different properties, and this ADR's first draft conflated them.

Milestone 3 owns implementation; its exit gate covers capability separation, scope validation,
revocation reaching live control, the `is_admin` root, and the gate on both administrative routes.

## Verification and rollback

Verified by authorization tests over the five redemption checks, a revocation test asserting that
an in-flight operation leaves its trajectory `abandoned` rather than `failed` or `completed` on a
turn carrying `close_reason = grant_revoked`, and multi-Subject tests confirming an `observe` grant
cannot reach any control route.

One test is specifically about decision 7 and would have caught the conflation: two Subjects with
different grants on one environment must receive **byte-identical** `link` facets and differing
`controlBlock` values. A facet that varies by caller fails it, which is the assertion the first
draft's shape could not have passed. Two more cover the authority root: an `is_admin` Subject can
revoke and unpair a `server` environment while reading no session it holds no `observe` grant on,
and a `machine` environment refuses both routes to a browser session presenting no local
confirmation code.

Rollback before multi-user data exists is removal of the grant tables, the `is_admin` flag, the
capability filter, and the two gated routes, returning to the single-Subject bootstrap. Session
identity is untouched either way.
