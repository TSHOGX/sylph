# API — Sessions, turns, links, and requests

The session lifecycle: creating one, dispatching turns, reading the event stream,
answering what an agent is blocked on, and reading lineage. `docs/api/README.md` holds
the conventions every endpoint here assumes.

## Sessions

### `GET /api/v1/sessions`

One list, one store. There is no live/archived split and no deduplication step,
because native history is parsed into the same events a live turn emits and lands in
the same store (`docs/architecture.md` § One authority).

**Query parameters:** `limit` (default 50, max 200), `agent`, `environment`,
`workdir`.

**Response 200:**
```json
{
  "sessions": [
    {
      "id": "env7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
      "environment": "env7fk2",
      "agent": "claude",
      "nativeId": "9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
      "workdir": "/Users/hsw/Workspace/project",
      "title": "Feature implementation",
      "injectIdentity": true,
      "fidelity": "stream",
      "flagsJson": "{\"model\":\"opus-5\",\"effort\":\"high\"}",
      "status": "idle",
      "createdAtMs": 1724054400000,
      "updatedAtMs": 1724058000000
    }
  ]
}
```

`id` is the encoded triple and the route key. `environment`, `agent`, and `nativeId`
are also given as separate fields **so a client never parses the id**
(`docs/api/README.md` § A session id in a path is the identity triple).

The environment display name is resolved from `GET /api/v1/environments` rather than
duplicated on every session row. It is mutable metadata, never a key: a rename must not
destroy identity (`docs/environments.md` § Pairing).

**`fidelity` is what a renderer must read before drawing a duration.** `stream` means
every delta and its arrival time is recorded; `blocks` and `messages` mean the source
recorded less, and no care recovers a timing that was never written. A waterfall over
message-fidelity data presents equal-length bars as measurements.

**`flagsJson` is a JSON *string*, not a `flags` object.** Requests take `flags` as an
object; responses report the stored column and the client parses it. This document
showed `flags` here for a release, so a client written from it read `undefined`.

**A path filter is an exact match after normalizing trailing slashes, never a
substring.** The store this replaced compiled `workdir` to `LIKE '%' || ? || '%'`, so
asking for `/w/app` also returned `/w/app-old` and every subdirectory.

### `POST /api/v1/sessions`

**This starts an agent.** It does not reserve a durable row. A session has no durable
identity until an agent has assigned one (`docs/architecture.md` § Identity), so the
pre-message state is an in-memory draft rather than a session record.

**Request:**
```json
{
  "environment": "env7fk2",
  "agent": "claude",
  "workdir": "/Users/hsw/Workspace/project",
  "title": "Feature implementation",
  "injectIdentity": true,
  "flags": {"model": "opus-5", "effort": "high"}
}
```

`injectIdentity` decides whether `SOUL.md` is appended, defaults to `true`, and is
**refused by `PATCH`**: a transcript records what produced it, so rewriting the flag
would rewrite what the stored history means. It is a default, not a policy — see
`GET /api/v1/soul` for the text it governs.

**Response 201:** a process-local `draftId` plus the launch metadata.  This is not a
session id and is not listed anywhere.  The draft may still read or change its
working-directory files through the `/sessions/draft/{id}/files/*` routes, because the
prepared link already owns the contained `workdir`; those routes disappear with the
draft and never create history.  The first non-empty turn sent to
`/sessions/draft/{id}/turns` atomically promotes the link and returns the durable session.

**What changed, and what it deletes.** The previous shape wrote a row with a
server-minted UUID and left the agent unstarted, which produced a class of session with
no work in it. Three things go away with that: `DELETE /api/v1/sessions/{id}` for
empties, the frontend probe that called it on unmount, and the window in which a
session existed with no native id and therefore could not be resumed. A failed spawn now
reports at the start screen instead of leaving a dead row behind.

**`workdir` is bounded, and the bound now lives in the envagent.** It must resolve
inside that environment's `browse_roots` or its Quick Start directory, checked on the
canonical path. Only the machine holding the path can canonicalize it, know whether a
component is a symlink, or resolve `..` — a server checking a remote path would be
matching a string against a filesystem it cannot see (`docs/configuration.md`).

The reason it is bounded at all: this field is the `current_dir` of a spawned agent, and
a coding agent reads and writes the tree it is pointed at, so it decides **where
arbitrary code runs**. Existence — the only property it was once checked for — says
nothing about whether it should.

The canonical path is recorded by the envagent that owns the filesystem, so two
spellings of one directory do not become two workspaces. The server never canonicalizes
or compares a remote path against its own filesystem. Quick Start is sent as the
environment-level `__sylph_quick_start__` selector and resolved locally by that envagent.

**Errors:**
- `400` — unknown agent, workdir missing or not a directory, flags validation
- `403` — `POLICY_VIOLATION`: the workdir exists but is outside that environment's
  permitted roots. Distinct from `400` on purpose — the request is well-formed and the
  fix is in configuration, not in the request. The message lists what is permitted.
- `502` — `AGENT_START_FAILED`: the CLI could not be started or did not answer the
  initial capability handshake. A draft may start without a native id; identity is
  acquired and checked only when its first real turn is promoted.
- `503` — `ENVIRONMENT_UNREACHABLE`: no envagent is connected for that environment

A session already recorded outside the permitted roots stays fully readable. Nothing
re-checks a recorded workdir on read, list, or resume — a rule that applied on read
would make every session created under wider configuration unreadable, which is a worse
failure than the hole it closes.

### Prepared-link routes

The `draftId` returned by `POST /sessions` is an opaque, process-local handle. It is
valid only while the prepared link is alive and must never be passed to a durable
session endpoint. These routes expose the capabilities needed to configure a link
before the first message, without inventing a second session identity:

- `GET /api/v1/sessions/draft/{id}` returns the launch metadata (`draftId`, environment,
  agent, workdir, title, and `injectIdentity`).
- `GET /api/v1/sessions/draft/{id}/capabilities` returns the same typed `LinkState`
  surface used by durable sessions. The adapter owns all vendor-specific model,
  thinking, permission, and command details.
- `POST /api/v1/sessions/draft/{id}/config` accepts `{ "id": "...", "value": ... }`
  and returns the updated `LinkState`. The id and value are opaque to the server; the
  adapter validates and translates them.
- `POST /api/v1/sessions/draft/{id}/turns` accepts the normal turn payload. A non-empty
  text, declared command, or attachment is the promotion boundary: the adapter first
  obtains the vendor's native id, then the server atomically writes the durable session
  and first turn and returns `SessionEnvelope`. An entirely empty payload is rejected
  and leaves the draft disposable.
- `DELETE /api/v1/sessions/draft/{id}` closes the link and removes draft-only temporary
  resources. It always leaves no journal row; calling it for an already-discarded draft
  is harmless.

Remote draft promotion is serialized per draft. A retry carrying the same
`idempotencyKey` returns the original session after the first promotion; a
different key conflicts rather than sending a second non-idempotent first turn.

Draft file routes under `/sessions/draft/{id}/files/*` use the same containment rules as
durable file routes and disappear with the draft. Reading or changing files never
promotes a draft and never creates history.

### `POST /api/v1/sessions/adopt`

Bring in a session an agent already has on some environment, from that agent's own
native store. Replaces `POST /sessions/import`.

**Request:**
```json
{
  "environment": "env7fk2",
  "agent": "claude",
  "nativeId": "9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
  "watch": true
}
```

`watch: true` follows the store as it grows, so a session running in a terminal right
now appears and keeps updating; `false` parses one pass of what is already recorded
(`docs/environments.md` § Watching and adopting). `title` may be supplied to override
the derived one.

**Response 201:** the session object, with `fidelity` reflecting what the vendor's store
actually holds — `blocks` or `messages`, never `stream`.

**Policy-relevant fields are derived from the store, not from the request.** The
working directory comes from the session's own recorded environment context; the title
falls back to the opening user turn. `injectIdentity` defaults to **false** here,
because the transcript so far was produced without it — but it is settable, since
adopting a terminal session and injecting from that point on is a coherent thing to
want.

**The recorded working directory is bounded exactly like a created one**, and this is
not belt-and-braces. A machine's store holds every session any agent ever ran there, so
an unbounded adopt is a *general* way to start an agent anywhere: find a session that
ran in the target directory, adopt it, send a turn. That the path came from the store
rather than from the request changes who chose it, not what happens next.

**The title fallback must skip injected envelopes.** The first `user` event is usually
not what the user typed — agents inject their own context as leading pseudo user turns
(`<environment_context>`, `<skills_instructions>`, memory files, `claude`'s `Caveat:`
preamble). Taking it verbatim labels most sessions with boilerplate byte-identical
across every session of that agent, so skipping those envelopes is what makes a label
recognizable at all. Keep the rule narrow: treating a leading `You are ` as boilerplate
discards genuine labels, because subagent briefs legitimately open that way.

**Errors:**
- `400` — no such agent on that environment, or the recorded workdir no longer exists
- `403` — `POLICY_VIOLATION`: the recorded workdir is outside that environment's roots
- `404` — `NATIVE_SESSION_NOT_FOUND`: that environment's store has no such session
- `409` — `SESSION_ALREADY_PRESENT`: adopting twice would produce two rows for one
  conversation
- `422` — `NATIVE_SESSION_NOT_RESUMABLE`: the store recorded no working directory
- `503` — `ENVIRONMENT_UNREACHABLE`

Those are distinct because they have distinct fixes: nothing to do, widen
configuration, wrong id, already done, unusable source, and bring the machine online.
Collapsing any pair sends a reader to the wrong layer.

### `POST /api/v1/sessions/{id}/reread`

Re-parse the vendor's native store for a session Sylph already holds, and ingest whatever
the journal does not already have. One pass, on demand
(`docs/environments.md` § Re-reading a session Sylph already holds).

This is what a "refresh transcript" control calls. It does **not** start or restart a
follower, and it does not change the session's `fidelity`.

**Request:** no body.

**Response 200:**
```json
{ "reread": { "examined": 412, "ingested": 37, "alreadyHeld": 375, "readErrors": 0 } }

### `POST /api/v1/sessions/{id}/reproject`

Re-projects native envelopes already retained by Sylph through the owning envagent. The
vendor's source files are not read, so the operation remains available after rotation or
deletion of the native store. The response uses the same `RereadReport` shape; `examined`
counts retained projected events and `ingested` counts new events added to the journal.
```

Every field is a count of events in this one pass:

| Field | Meaning |
| --- | --- |
| `examined` | events the adapter emitted, before suppression |
| `ingested` | events new to the journal, now added |
| `alreadyHeld` | events a previous pass had already journaled |
| `readErrors` | items the adapter could not read; the pass continues past each |

**`ingested + alreadyHeld` may be less than `examined`, and a client must not compute
one from the others.** A turn's opening bracket survives in neither storage tier, so a
pass that finds no new content after one neither writes it nor claims it was already held
— it is examined and nothing else
(`docs/environments.md` § Re-reading a session Sylph already holds). The difference is
bounded by the number of brackets that bracketed nothing, so it is normally zero.

`ingested: 0` with a non-zero `examined` is the ordinary "nothing new since last time"
answer and is a success. **A client must not treat it as a failure**, and must not infer
"the store is gone" from it — that case is a `404`. A caller refetching
`GET /sessions/{id}/turns` after a `200` sees whatever the pass added; the response is
returned only once ingestion has finished, so there is nothing to race.

**Errors:**
- `400` — `INVALID_REQUEST`: the id is malformed, or that agent is not installed on the
  environment holding the session
- `404` — `SESSION_NOT_FOUND`: Sylph holds no session with that id
- `404` — `NATIVE_SESSION_NOT_FOUND`: Sylph holds the session and the vendor's store no
  longer does, because it was pruned or rotated away
- `500` — `INTERNAL`: the store could not be read, or a record could not be retained
  losslessly
- `503` — `ENVIRONMENT_UNREACHABLE`: no envagent is connected for that environment

The two `404`s are one status and two codes on purpose: "you named nothing" and "the
vendor dropped what you named" have different fixes, and only the second means the
transcript Sylph holds is now the only copy.

### `GET /api/v1/sessions/enumerate`

What native sessions an environment holds, without adopting any of them. This is what
makes adopt offerable in a UI rather than requiring a user to know a vendor id.

**Query parameters:** `environment` (required), `agent`, `workdir`, `limit`, `cursor`.

Answered by the envagent from its local stores. A session already adopted is marked as
such, so a picker can show it rather than offering a duplicate that would `409`.

### `GET /api/v1/sessions/{id}`

Get one session.

**Response 200:**
```json
{ "session": { /* the same shape a listing row has, below */ } }
```

**That is the whole response.** `api::sessions::get` returns `SessionResponse { session }`
and nothing else — no turn summary and no turn counts, on this or any other route. A client
that needs the turn list asks `GET /sessions/{id}/turns`.

The response exposes the vendor id as `nativeId`, alongside the encoded `id` and the
separate `environment` and `agent` fields. There is no second server-minted id to
translate, and session timestamps are `createdAtMs` / `updatedAtMs`.

### `PATCH /api/v1/sessions/{id}`

Change agent flags or the title of an existing session.

```json
{ "flags": { "model": "opus", "effort": "high" }, "title": "Auth refactor" }
```

Both fields are optional; an omitted field is left untouched. `flags` is a
**complete replacement set**, not a merge — the caller has already validated the
whole set, and merging would let a stale key survive a change the user believed
was total. An empty object clears them.

Note the asymmetry: `flags` is what a *request* carries, while a session
*response* reports `flagsJson`, a JSON string the client parses
(`models/session.rs`, `frontend/lib/api.ts:42`). Reading a session and echoing its
`flags` back is therefore not a round trip.

**Why flags are validated here rather than at turn time.** `POST .../turns` filters
the stored set against the agent's declared flag schema and silently drops what it
does not recognize. That is right there: a descriptor can change under a session
created before it, and the turn should still run. It is wrong for a user action —
a model chosen in the composer that the agent never declared would be accepted,
stored, dropped at turn time, and the turn would run on the default with nothing
anywhere saying why. Validating on write turns that silent no-op into an error the
UI can show against the control that caused it.

**Identity is not patchable.** `environment`, `agent`, and `nativeId` *are* the
session's identity, so changing one does not modify a session, it names a different one.
`injectIdentity` is refused for a different reason: a transcript records what produced it,
so rewriting the flag would rewrite what the stored history means. There are no `origin`,
`identityMode`, or `lifecycleMode` fields — `docs/architecture.md` § One class of session
is authoritative for
them.

Errors: `SESSION_NOT_FOUND` (404), `INVALID_REQUEST` (400) for an undeclared flag
key, a value outside a declared enumeration, or an agent that is no longer
installed.

### There is no `DELETE /api/v1/sessions/{id}`

**Nothing deletes a durable session.** A prepared draft is disposable through its draft
route because it has no journal row; no route deletes a session *with* history. Delete,
move, and export of durable records are absent; see the roadmap.

## Turns

### `POST /api/v1/sessions/{id}/turns`

Create a turn (send message or attachment).

**Request:**
```json
{
  "text": "Implement feature X",
  "attachments": [
    {
      "name": "screenshot.png",
      "type": "image/png",
      "data": "base64_encoded_content"
    }
  ],
  "idempotencyKey": "client_generated_uuid"
}
```

**Response 202:**
```json
{
  "turn": {
    "id": "t_ghi789",
    "sessionId": "env7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
    "status": "queued",
    "requestText": "Implement feature X",
    "createdAtMs": 1724059200000
  }
}
```

**Errors:**
- `404` — session not found
- `400` — validation (text empty, attachment too large, bad base64)
- `409` — duplicate idempotency key

Limits:
- Max 5 attachments per turn
- Max 8 MiB per attachment
- Max 10 MiB total per turn

### `GET /api/v1/turns/{id}`

Get turn detail.

**Response 200:**
```json
{
  "turn": {
    "id": "t_ghi789",
    "sessionId": "env7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60",
    "status": "completed",
    "origin": "user",
    "requestText": "Implement feature X",
    "commandInvocationId": null,
    "idempotencyKey": "…",
    "errorCode": null,
    "errorMessage": null,
    "createdAtMs": 1724059200000,
    "startedAtMs": 1724059205000,
    "completedAtMs": 1724059312000,
    "stoppedAtMs": null
  }
}
```

**`origin` is `user` or `agent`, and a client that ignores it renders an answer to a
message the user never wrote.** This is turn-level provenance, not a session class. An agent may start a turn of its own to report work that
outlived the turn which dispatched it, and such a turn's `requestText` is empty — which
is otherwise indistinguishable from a user sending nothing. `docs/session-protocol.md`
§ An agent may start a turn.

**There is no per-turn exit code**, because an adapter holds a session for its lifetime and
a turn is a message on its link. A turn's outcome is `status` plus
`errorCode`/`errorMessage`; nothing reads a process exit.

### `GET /api/v1/turns/{id}/events`

Stream turn events via SSE.

**Query parameters:**
- `after_seq` (default -1, replay from this sequence number)

**Response headers:**
- `Content-Type: text/event-stream`
- `Cache-Control: no-cache, no-store, no-transform`

`no-transform` (RFC 9111 §5.2.2.6) is required, not decorative. It forbids an
intermediary from re-encoding the body, and without it the Next.js proxy's
`compression` middleware gzips the stream — which cannot be decoded until a block
flushes, so the client receives the entire turn at once instead of incrementally.
Exactly one `cache-control` header is sent; two would leave the applied directive
up to whichever an intermediary reads first.

Note that each frame is **named** after its event kind (`event: thinking`), so a
client must not dispatch on `EventSource.onmessage`, which by spec receives only
frames named `message`.

**Response 200 (text/event-stream):**
```
event: thinking
data: {"seq":0,"phase":"started","text":""}

event: thinking
data: {"seq":1,"phase":"delta","text":"Analyzing"}

event: thinking
data: {"seq":2,"phase":"completed","text":"Analyzing the codebase"}

event: assistant
data: {"seq":3,"phase":"delta","text":"I'll implement"}

event: assistant
data: {"seq":4,"phase":"completed","text":"I'll implement feature X..."}

event: tool_call
data: {"seq":5,"phase":"started","name":"Write","streamId":"tc_1"}

event: tool_call
data: {"seq":6,"phase":"completed","name":"Write","text":"file_path: src/feature.rs\n..."}

event: usage
data: {"seq":7,"data":{"inputTokens":1234,"outputTokens":567}}
```

Every event is ordered by `seq`, and carries `arrivalAtMs` (the server receipt clock) plus
optional `recordedAtMs` (the vendor clock). A duration may subtract only endpoints in the
same clock domain.

Delegated work carries `span`, naming the trajectory an event happened in; absent
means the root turn. A `subagent` event's `streamId` *is* a trajectory id, so a
nested delegation is one whose `span` names another — which is how a subagent, a
subagent's subagent, and a dynamic workflow's parallel phases all travel in one
flat stream. See `docs/session-protocol.md` § Delegated trajectories.

```
event: subagent
data: {"seq":8,"phase":"started","streamId":"s1","name":"Explore"}

event: tool_call
data: {"seq":9,"phase":"completed","streamId":"t7","span":"s1","name":"Grep"}

event: subagent
data: {"seq":10,"phase":"started","streamId":"s2","span":"s1","name":"Worker"}

event: subagent
data: {"seq":11,"phase":"completed","streamId":"s1"}
```

Unlike other kinds, a `subagent` event is `stable` at every phase: its `started`
phase is where a trajectory's identity, name, and parent live, so dropping it as an
ephemeral projection would leave the journal holding children that point at a parent
it never recorded. A trajectory an adapter never closes is closed by the server when
the turn terminates, carrying `data.closedBy = "server"` — which a client reports as
*abandoned* rather than *failed*, because nobody said it failed, only that nobody
said.

SSE reconnection:
- Client sends `Last-Event-ID: {seq}`
- Server replays from `seq+1`
- Stable events replayed from journal
- Ephemeral events replayed from bounded projection

### `POST /api/v1/turns/{id}/stop`

Stop a running turn.

**Response 202:**
```json
{
  "turn": {
    "id": "t_ghi789",
    "status": "stopping",
    /* ... */
  }
}
```

**What happens, in order.** A turn runs inside a session-long link rather than its own
process, so **nothing signals or kills a process to stop a turn** — that would end the
whole session to stop one message:

1. **The turn is settled first**, as `Interrupted`, before the link is touched at all.
   This ordering is the point rather than an optimization: every way of ending a turn
   asks the agent to stop producing, and an agent that stops producing is one its
   drain task observes closing — which reports `LINK_LOST`. Claiming the outcome first
   means that attempt finds the turn already terminal and does nothing, so a
   deliberate stop is never reported as a transport failure. Settling *after*
   interrupting is a race rather than a fix.
2. **The adapter is asked to interrupt.** Settling first is also what makes stop
   unconditional — the record does not wait on the agent to answer, so a user asking
   to stop is never told the agent declined.
3. **If interrupt fails or is unsupported, the link is torn down.** That costs the
   session's live context, which is why it stays the fallback rather than the path.
   Link teardown itself is `close` on the wire, a 500 ms grace period, then `SIGTERM`
   to the process group; escalation is unconditional because signalling an
   already-exited group is defined to be harmless. There is no `SIGKILL` step.

## Links

A **link** is a live channel to a session's agent, held by the envagent for the
environment that session runs in. It is not the session: a session with no link is
complete, resumable, and searchable, and links are torn down on idle (30 minutes) and do
not survive a server restart or an envagent disconnect. Everything in this subsection is
therefore about *live* state, and a client must render it as live state rather than
promising it unconditionally.

These five routes had no section in this document at all until now, which is why they
are grouped rather than filed under Sessions: `capabilities`, `link`, `interrupt`,
`steer`, `config`, plus the session event stream below.

### `GET /api/v1/sessions/{id}/capabilities`

Returns `LinkState`. **Establishes a link as a side effect** — this is the read that
most clients use to attach, because a UI asking what a session can do is a UI about to
offer it.

A failure here is **200, not 5xx**, carrying an unavailable `control` facet with a
typed reason.
That is the endpoint's one non-obvious decision and it is deliberate: a missing adapter
or a refusing agent is the *agent's* failure, not the request's, and the session remains
valid with readable history — a 500 would make the whole session look broken. The two
facets make the reason explicit instead of flattening it into an unexplained boolean.

Two consequences a client must handle:

- **Do not cache this response indefinitely.** A detached-with-reason 200 under
  `staleTime: Infinity` makes one failed handshake permanent for the life of the page.
  Cache by reason: `no_agent` is cacheable like a success, because no retry installs a
  vendor CLI; a failed handshake must be allowed to go stale.
- **An empty capability surface means "Sylph cannot reach it", never "there is nothing
  there".** These coincide for a session Sylph launched and are opposite for one running
  in a terminal, where the process is alive and simply not ours to drive.

**`LinkState` carries two typed facets, `observe` and `control`, each able to report
unavailable with a reason.** This is the shape rather than a third link state, because
the vendors do not sort into three buckets: a terminal `pi` is observable and
undrivable, `codex` is both, `claude` is observable and semi-drivable behind a human
approval gate (`docs/agents.md`). Two failure codes worth naming, because neither
existed and both are the common case now:

| Code | Means |
| --- | --- |
| `environment_unreachable` | no envagent connected for this session's environment — observe still answers from the store |
| `not_ours_to_drive` | the process is alive under someone else, and this vendor offers a second party no control channel |

**These differ in whether a retry can change the answer**, and a client must not cache
them alike. `not_ours_to_drive` and `no_agent` are cacheable like a success — no retry
teaches a vendor a control channel it does not have, or installs a CLI. But
`environment_unreachable` **must be allowed to go stale**, because a laptop wakes and an
envagent redials; cache it like a success and a reachable machine goes on being reported
offline for the life of the page.

`crates/protocol`'s `UnavailableReason::is_retryable` is the single definition, and a
client mirrors it rather than re-deriving the rule inside a cache policy. The vocabulary
itself is authoritative in `crates/protocol/src/link.rs`.

**That policy is sound only because a facet describes the environment rather than the
caller**, and this is where a facet's scope stops being an abstract distinction. Every
reason above is observer-independent, so caching one is caching a fact about a machine.
The caller's own authorization is a sibling field and is **never cached** — a withdrawn
grant held under the rule above would be a denial outliving the grant it describes, and a
newly issued grant would not take effect until the tab reloaded
(`docs/architecture.md` § A facet describes the environment, never the caller).

So the response carries three fields with two cache rules:

| Field | Shape | Cache |
| --- | --- | --- |
| `link.observe`, `link.control` | `{available, reason?}` with `reason` an `UnavailableReason` | per `is_retryable` |
| `authorization` | absent when the caller holds `control`; otherwise `not_granted` or `expired` | never |
| `controlBlock` | absent when control is admitted; otherwise `{kind, reason}` with `kind` of `unauthorized` or `unavailable` | never |

`controlBlock` is derived on the server and reports authorization **before** reachability —
not a severity ranking, but a disclosure rule: an unauthorized caller learns nothing about
whether someone else's machine is awake. A client renders `controlBlock` and does not fold
the other two itself, because one fact with two derivations diverges the moment either
grows a case.

`authorization` distinguishes `not_granted` from `expired` because the remedy differs — a
first issuance against a renewal — and stops there. It does **not** distinguish "revoked"
from "never granted": revocation deletes the row
(`docs/database-schema.md` § Subjects, environments, and grants), so the two are indistinguishable in the store and
a third value would be a claim the store cannot support. What revocation did stays on the
turn it cut short, as `close_reason = grant_revoked`.

### `POST /api/v1/sessions/{id}/link`

Establishes a link eagerly and returns `LinkState`. Worth its own operation because
establishment is slow — an agent CLI loads a project — so a UI that knows a user is
about to type can pay that cost before they finish rather than after they send.

Unlike `capabilities`, a failure here **is** an error (`LINK_FAILED`), because
establishing was the entire request rather than a side effect of one.
`ENVIRONMENT_UNREACHABLE` (503) is separate from `LINK_FAILED` (502): one is fixed by
bringing a machine online, the other by looking at the agent.

### `DELETE /api/v1/sessions/{id}/link`

Tears the link down without ending the session, and returns the resulting `LinkState`.
History is unaffected. The next turn re-establishes.

### `POST /api/v1/sessions/{id}/interrupt`

Asks the agent to stop producing. Session-scoped rather than turn-scoped, and so are
`steer` and `config`: all three act on the *agent*, and the session is what holds a
link. A turn-scoped route would force a caller to know a turn id in order to interrupt
the only turn running.

### `POST /api/v1/sessions/{id}/steer`

```json
{ "text": "actually, use the existing helper", "turnId": "t_abc" }
```

`turnId` is optional, because a client with no turn in view is steering "whatever is
running", which is a coherent ask rather than a missing field.

### `POST /api/v1/sessions/{id}/queue`

```json
{ "text": "then run the tests" }
```

Answers `{"queued": true}`. Requires `Capability::Queue`; an agent without it answers
`UNSUPPORTED_FIELD`, which a client renders by retiring the control rather than leaving
it clickable.

**Not `steer` with a flag**, and the difference is *when the agent reads the text*
rather than how it is delivered. A steer is a correction, so it carries a turn
precondition: text that misses its turn lands in the wrong conversation, and naming the
turn is what turns that into a refusal. A queued follow-up is *for* after the turn, so
it has no such precondition — a turn boundary crossing underneath it changes nothing,
and queueing with nothing running is a coherent ask that becomes the next turn's opening
text. Hence no `turnId` here at all, where one endpoint serving both meanings would need
a field that is required for one and meaningless for the other.

That shape is what let a vendor claim the wrong word for what it does: `claude` declared
`Steer` while its only mid-turn channel is stdin, which `--input-format stream-json`
reads through one iterator the turn loop awaits — so the text is not read until the turn
ends. A user's correction arrived after the thing it was meant to prevent, with every
layer reporting success. All three agents have `Queue` and each spells it differently;
only `codex` and `pi` also have a real steer (`docs/agents.md` § Capability matrix).

### `POST /api/v1/sessions/{id}/config`

```json
{ "configId": "model", "value": "anthropic claude-opus-5" }
```

A typed set, so `/model` and `/permissions` are not prose sent as a prompt. Four rules
this endpoint's history earned, each of which cost a debugging round:

- **`value` is opaque.** It is a token the agent enumerated and the client echoes back,
  never something to parse. `pi`'s model ids contain `/` in 350 of 372 cases, so the
  composite encoding uses a space — and only because the value is opaque can that
  encoding change without a client change.
- **A refusal distinguishes two codes.** `UNSUPPORTED_FIELD` means no such option and
  justifies disabling the control; `INVALID_VALUE` means *this value, right now* and
  must leave it usable. Flattening both into one retires a picker whose other values
  work.
- **A client must render the refusal.** A mutation with only an `onSuccess` path leaves
  a rejected set invisible: the cached surface is untouched, the control re-renders from
  it, and the sole observable outcome is that clicking did nothing. Every layer behaves
  correctly and the symptom points at the one that is not at fault.
- **The reported `currentValue` comes from the vendor's confirmation, never from the
  request.** A vendor's `success` is not a promise it did what you asked: measured on
  `claude` 2.1.246, `manual` answers success while confirming `default`.

An accepted setting is remembered for a durable session only when its adapter declares a
safe launch-flag equivalent. That mapping belongs to the adapter: the server does not
know that Claude's live `permissionMode` is spelled `mode` at launch, nor should it persist
runtime-only controls such as output style. The stored `flagsJson` therefore represents
the next-link launch preferences, while `currentValue` remains the vendor-confirmed live
state. A prepared draft keeps the same adapter-owned subset in memory until promotion.

### `GET /api/v1/sessions/{id}/events`

Session-scoped facts as SSE: a link attaching or dying, capabilities changing, a request
raised or retired. Distinct from the turn stream, which exists only while a turn does —
these happen with no turn running, which is precisely when a client would otherwise have
no way to learn them.

Two properties worth relying on. A **pending request is published on every transition**,
including its departure, from one method rather than three call sites — a set that only
ever grew is what made an answered question sit on a second client until reload. And the
payload is the **row**, carrying `status`, not the transition, so a client reconciles
against a state rather than replaying a vocabulary of verbs, and a status added later
needs no client change.

### `GET /api/v1/sessions/{id}/trace/events`

The reconnectable, session-scoped trace. This is the stream a transcript view should
hold while a session is open: it carries normalized events from every turn, not only
link/request invalidations. Stable rows are replayed from `journal.db` or
`history.db`; the live channel contributes the bounded ephemeral tail and then stays
open across turn boundaries.

Each event includes `arrivalTurnId`, the turn on which the server received it. That is
not trajectory ownership: a child can finish after its parent turn, so the frontend's
trace index uses the first trajectory declaration (or promoted host) as `ownerTurnId`
and keeps `arrivalTurnId` only as provenance. This is what prevents a late Codex child
close from becoming a synthetic `origin: agent` turn or a second outer card.

The older `/sessions/{id}/events` endpoint remains a cheap state-invalidation stream
for capabilities, links, and pending requests; it intentionally has no trace replay.

### `GET /api/v1/sessions/{id}/replay`

Server-paced reconstruction of a session's history. It is session-scoped and lives on
the server for one reason: reconstruction is subtle enough that doing it per frontend
would let two frontends disagree about what the same history means.

## Requests

The inversion: the agent asks, and a human answers. A request is a resource rather
than a stream message, because the agent blocks until it is answered — so it has to
outlive every client and be answerable from a frontend that attached an hour later.
`docs/session-protocol.md` § Requests: the agent asks, a human answers governs the wire shape.

### `GET /api/v1/sessions/{id}/requests`

What this session is waiting on, oldest first — read from the journal rather than
from the link, so it answers after a reconnect, from a different frontend, and for a
session whose link has since died. That last case is exactly when a user needs to
know why nothing is happening.

**Response 200:**
```json
[
  {
    "id": "req_abc123",
    "sessionId": "s_def456",
    "turnId": "t_ghi789",
    "kind": "choice",
    "tool": "AskUserQuestion",
    "title": "Two questions",
    "detailJson": "{}",
    "optionsJson": "[{\"id\":\"submit\",\"label\":\"Send answer\"},{\"id\":\"deny\",\"label\":\"Don't answer\"}]",
    "formJson": "{\"fields\":[{\"type\":\"select\",\"id\":\"q:0\",\"mode\":\"one\",\"choices\":[{\"value\":\"Tabs\"},{\"value\":\"Spaces\"}],\"allowOther\":true,\"notes\":{}}]}",
    "defaultOption": "deny",
    "risk": "safe",
    "allowPersistent": true,
    "status": "pending",
    "createdAtMs": 1730000000000
  }
]
```

`formJson` is the input the request declared it wants collected. Absent means an
approval, whose subject is the one proposed tool input in `detailJson`
(`docs/session-protocol.md` § Request forms), which is why it is declared rather than
inferred.

The adapter's own request id is never serialized. A client that could send it could
answer a request that was never raised.

### `POST /api/v1/requests/{id}/resolve`

Answer one. Addressed by request id at the top level rather than under a session,
because the id is what the answer is *about*.

```json
{
  "option": "submit",
  "input": {
    "values": {"q:0": "Tabs", "q:1": ["ESLint", "Clippy"]},
    "notes": {"q:0": "only for generated files"}
  }
}
```

For a request with no form, `input` is instead an edited version of what the agent
proposed — how "allow, but change the command" works. Absent means "as proposed".

**Response 200** carries the resolved request.

| Status | Code | When |
| --- | --- | --- |
| 400 | `INVALID_REQUEST` | An option the agent never offered, or an answer that does not fit the declared form |
| 404 | `REQUEST_NOT_FOUND` | No such request |
| 409 | `REQUEST_ALREADY_RESOLVED` | Someone else answered first |
| 409 | `LINK_GONE` | The answer was recorded, but no live agent is waiting for it |

Both validations exist for the same reason: an unrecognized value reaches the vendor
CLI, and what happens there is undefined — for an approval, "undefined" must never be
able to mean "allow". Validation happens *before* the row is claimed, so a refused
answer leaves the request open and answerable rather than consuming it.

The claim is a single `UPDATE ... WHERE status = 'pending'`, so two clients answering
at once produce one winner; the answer reaches the adapter only after the claim
succeeds, which is what stops the loser also reaching it. `LINK_GONE` is a real
outcome rather than an error to hide: the user did answer, and saying nothing is
waiting beats a success that went nowhere.

## Lineage, folding, and labels

**There is no `/archive/*` namespace.** Reading history is `GET /sessions` and
`GET /sessions/{id}`, because one store holds it (`docs/architecture.md` § One authority);
what would be a separate read path is query parameters on the listing above.

### A listing folds delegated work into its parent

**Roughly half the sessions in a real store are delegated work**, so a listing that does
not fold them is half noise. That ratio has held across re-measurements while the absolute
counts moved, which is why the fold is a design rule rather than a tuning choice.

So a row carries `delegateCount` and `delegates`, and the fold has one exception that
is not optional:

**A cross-directory child is promoted to a row of its own.** 484 of 2238 delegates ran
somewhere other than their parent — typically a worktree or a subdirectory — and folding
those into a parent that is *not in the directory being listed* deletes them from the
only listing that would ever show them — measured, it emptied dozens of directories, one
of them holding hundreds of sessions. In its own directory a delegated session is not
delegated work, it **is** the work.

`delegates` carries the children themselves, and `delegateCount` is always its length.
A count alone is a dead end: half the store is delegated work, so a row reading "13
delegates" names sessions the client has no way to reach. The fold already visits every
child to count it, so identity costs nothing beyond the bytes. Each entry is
deliberately thinner than a listing row, and `cwd` appears only when the child ran
somewhere other than its parent — the case worth showing.

### A fork and a delegation are different relationships

`parent` names the ancestor and `relation` says which relationship it is, both read from
the edge that records it (`docs/database-schema.md` § sessions). They must not be presented
the same way:

- **Delegation** folds into the parent as a count.
- **A fork** replays its origin's entire prefix and continues past it, so the two share
  every early user turn and read as one conversation resumed. Listing both shows it
  twice under one label, so **one survives** — and which one cannot be decided in advance:
  of two real local cases, one had the bulk of its later work in the fork and the other in
  the origin. Ordering by end time descending makes the survivor whichever is reached
  first.

`forkOf` names the suppressed origin on a surviving fork row. Within one response the
loser is already dropped, but **a pair can straddle two pages** — measured pairs sit
hundreds of rows apart — and the server keeps no state between requests. A client accumulating
pages must therefore drop a row whose `forkOf` names a session it has already shown, and
equally a row that a previously-shown fork names.

### Lineage is derived from the listing, not asked per row

**Per-row lineage lookups are two orders of magnitude more expensive per fact than
reading the listing**, measured against a full local store: one session's metadata costs
milliseconds and a few kilobytes, so a single page spends a substantial fraction of a
second on lineage alone and a whole-store scan takes tens of seconds — while the entire
listing that already carries most of those facts returns in a fraction of one.

So **derive lineage from the listing and ask per row only for what the listing cannot
carry.** The fold caches per parent for the same reason. Finding one session's children
uses an id-prefix test over a single pass, which catches most delegates; the remainder
record only a parent id and are reached from their parent's directory listing rather than
by paying for the scan.

Re-derive these figures before relying on them (`docs/README.md` § How to write these
documents). The *ratio* is the durable part, and it is what the design rests on.

### An id-suffix convention is a fallback, not a lineage signal

Some agents name a delegated session with a suffix derived from its parent, and that
catches most of them. It **misses those that record only a parent id** — `codex` among
them, measured from its own store: a rollout with `parent_thread_id` set,
`thread_source: subagent`, and no suffix at all. Those sat at the top level of every
listing until lineage started reading the store's own recorded parent. The suffix
survives only for when that lookup fails.

Attribution to the *specific* delegating call was measured and **rejected**: `claude`
names a child `<parent>/agent-a<name>-<hash>`, but the large majority of those suffixes
are hash-only, so matching recovers a small fraction of delegations and misattributes on a
collision. Structure that is right for one delegation in ten is worse than an honest flat
view, because nothing on screen distinguishes the two. Delegation is therefore
session-level in both directions.

### A label falls back through three sources

1. **The vendor's own recorded title.** `claude` records one for ~73% of sessions;
   `codex` and `pi` record none — so the fallbacks are the common path, not the
   exception.
2. **The opening *typed* message**, skipping leading harness-injected envelopes
   (`<environment_context>`, `<skills_instructions>`, memory files, `Caveat:`). They are
   byte-identical across every session of an agent and identify nothing.
3. **An envelope's `summary` attribute.** Some delegated sessions have no typed turn at
   all — every one is a `<teammate-message … summary="…">` block — but that attribute is
   human-written and describes the task. XML entities in it are decoded.

Together those three label nearly every session in a real store; the title and typed ask
alone leave a visible fraction unlabelled, and rule 3 is what closes most of the rest.

### A paged scan is bounded in rows, not pages

A directory filter also matches paths beneath it, and those sort by recency rather than
depth, so a parent directory's own first exact match can land a thousand or more rows in —
past which a page cap gives up and reports a directory with dozens of sessions as
**empty**. Bound such a scan in rows examined.

## Search

```
GET /api/v1/search?q=<text>&limit=20&environment=<id>
```

FTS5 over the server's own history. It is the complement of the listing endpoints
rather than a variant of them: those answer "what happened in *this directory*", which a
sidebar asks on every render, while this answers a question a user has to already know
how to phrase.

**It covers everything the server holds.** The index is over the same rows the timeline
renders, including sessions read out of an agent's native files, so a client may present it
as "search my history" without qualification — the coverage is a contract rather than a
function of what some parser reached (`docs/architecture.md` § One authority).

**It never returns 503**, and answers `200` with an empty `results` array for a query
that finds nothing. Whitespace-only `q` is treated as absent and answers empty without
touching the index, so a client may bind the field directly to keystrokes.

`limit` defaults to 20. There is no pagination and deliberately no cursor: a hit is a
jumping-off point to a transcript, so the useful control is a narrower query, not a
deeper page.

**What a hit contains**, since a session is now identified by a triple and there is no
second store to name:

```json
{
  "results": [
    {
      "session": { "id": "env7fk2:claude:9bdacb6e-…", "workdir": "…", "…": "…" },
      "bestMatch": {"seq": 412, "turnId": "…", "excerpt": "…the matched text…"},
      "hits": 3
    }
  ]
}
```

`session` is the same object the listing endpoints return, so a client renders one row
component for both and never has a shape that exists only in search. `bestMatch` carries
the `seq` a client scrolls to and the excerpt that is the only text on the row containing
the query — which is why the search overlay keeps its own ranking rather than being
re-scored by a client-side list engine (`docs/frontend-architecture.md`). `hits` is the
count within that session, so one strong match and thirty weak ones are distinguishable.

Both `bestMatch` and `hits` are omitted rather than zeroed when the index cannot supply
them; a client must not read an absent `bestMatch` as "matched at the beginning".

**This is the one read that must stay off any polled path.** Every other operation
against this store is microseconds to sub-millisecond; a full-text query is two to three
orders of magnitude slower, and slower still under concurrent write load. FTS5 work is
real work and nothing about owning the store makes it cheaper, so the cost model that
governs every other read does not transfer here — **this endpoint is user-initiated, and a
client gates it on a real query rather than binding it to keystrokes.**
