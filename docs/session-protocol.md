# Agent Session Protocol (`sylph.agent.v2`)

The contract between the Sylph server and a pluggable agent. It replaces the
one-shot turn contract in the Sylph skill's `references/runner-protocol.md`,
which remains supported as a degraded path (see **Compatibility**).

## Why the old contract cannot be extended

The v1 contract is: pass the prompt on stdin, **close stdin**, read NDJSON from
stdout, trust the last line. Closing stdin is load-bearing there — it is how the
runner knows the prompt ended — and it is also what makes a native experience
impossible.

This was measured, not assumed. Running `claude -p --input-format stream-json
--output-format stream-json --permission-prompt-tool stdio` against a project
with an `ask` permission rule, the CLI sends a `can_use_tool` request back over
stdin and waits. With v1's `stdin.close()` the turn dies:

```
Tool permission request failed: AbortError: Stream closed
```

Holding stdin open, the same request arrives, is answerable, and the turn
proceeds. Every native capability follows from that one difference:

| Capability | Native channel | v1 result |
| --- | --- | --- |
| Approval prompts | `can_use_tool` request, awaits a reply | Stream closed; tool fails |
| Slash commands | `initialize` returns 46 real commands here | 1–2 hand-declared in `--describe` |
| Subagents | `task_*` notifications, `parent_tool_use_id` | Flattened into opaque tool calls |
| Interrupt | `interrupt` request, session survives | SIGTERM kills the session |
| Mid-turn steering | Further stdin messages | stdin already closed |
| Model / mode switch | `set_model`, `set_permission_mode` | Only at spawn |
| Context usage | `get_context_usage` | Unavailable |

The gap is not a missing feature. It is that v1 models a **turn** as the unit of
execution, and every agent CLI models a **session**.

## The unit is a session, not a turn

A turn is an event inside a conversation. v1 spawns one process per turn, so
anything the agent learned about *being in a conversation* — pending approvals,
loaded skills, permission decisions, subagent state — is discarded between
turns and rebuilt from a resume id.

v2 keeps a live process for the session and runs turns inside it. The process is
called a **link**, and a session is `attached` (a link is live) or `detached`.

A link is **not** required for a session to work. Links are expensive and do not
survive a server restart, so:

* A turn on a detached session **establishes a link first**, resuming the
  agent's native session by id. This is exactly v1's behaviour, and it is the
  floor the design degrades to rather than an error path.
* A link idles out (default 30 minutes) and is torn down. History is unaffected;
  the next turn re-establishes.
* Interactive capability is a property of the link, so the UI reports it as
  live state rather than promising it unconditionally.

This is what makes the browser experience match the terminal: while attached,
the same requests and notifications a TUI would render are available; while
detached, the session is still complete, resumable, and searchable.

## Shape

The server never speaks a vendor protocol. An **adapter** does, one per agent,
living where v1's runner lived:

```
server ──sylph.agent.v2 over stdio──► adapter ──vendor protocol──► agent CLI
         (normalized, this document)             (stream-json, JSON-RPC, rpc)
```

Keeping the adapter a separate process rather than a Rust module is deliberate,
and it is the one thing v1 got unambiguously right: adding an agent means
dropping in a directory, with no server build. It also contains a vendor's
protocol churn in a script the user can read and patch, and it keeps a crashing
adapter from taking the server down.

Adapters are discovered at `<runners_dir>/<name>/`. An adapter declaring v2
provides `adapter.sh`; a directory with only `runner.sh` is a v1 runner and is
driven on the degraded path.

## Framing

NDJSON in both directions on the adapter's stdin and stdout. One JSON object per
line, UTF-8, no embedded newlines. stderr is diagnostics, captured but never
parsed.

Every message carries `t` (type). Requests carry `id`, unique per direction for
the life of the link; responses echo it. Correlation is explicit because both
sides originate requests concurrently, and an implicit ordering would deadlock
the first time both spoke at once.

```json
{"t":"send","id":"c7","text":"fix the failing test"}
```

Three classes, distinguished by who must answer:

| Class | Direction | Answer |
| --- | --- | --- |
| **Command** | server → adapter | An `ok` or `err` echoing `id` |
| **Request** | adapter → server | A `resolve` echoing `id` |
| **Event** | adapter → server | None; fire and forget |

An unknown `t` is ignored with a warning, never fatal. That is what lets a newer
adapter talk to an older server: the protocol grows by adding message types, and
a peer that does not know one behaves as though the sender had not sent it.

## Handshake

The server opens the link, then negotiates before anything can go wrong:

```json
{"t":"hello","id":"h1","protocol":"sylph.agent.v2","server":"sylph-web/0.2.0",
 "accepts":["approval","dialog","subagent","plan","diff","context"]}
```

`accepts` is what the *server* can handle. An adapter must not send a request
kind absent from it — a server that cannot render approvals must never be handed
one, because the agent would block forever on a reply that no UI will produce.
This is the one place a capability mismatch could hang a session, so it is
negotiated rather than discovered.

The adapter replies with what it and its CLI can actually do:

```json
{"t":"ok","id":"h1","adapter":"claude/2.0.0","agent":{"name":"claude","version":"2.1.238"},
 "supports":["interrupt","steer","approval","subagent","slash","set_model","context","fork"],
 "flags":{"model":{"choices":["opus-5","sonnet-5"],"description":"..."},
          "effort":{"choices":["low","medium","high","xhigh","max"]}}}
```

`supports` is a closed vocabulary; unknown entries are ignored. It drives the UI
directly, which is why nothing in the frontend hardcodes an agent's abilities —
a composer that offers interrupt to an adapter that cannot interrupt is a lie the
handshake prevents.

`flags` carries v1's `--describe` schema unchanged, so an adapter has one place
to declare its options rather than two.

## Opening a session

```json
{"t":"open","id":"o1","workdir":"/Users/x/p","sessionId":"<native-id|null>",
 "soul":"/path/to/SOUL.md","flags":{"model":"opus-5"},
 "permissionMode":"ask","env":{"SYLPH_SESSION":"<web-uuid>"}}
```

`sessionId` is the agent-native id to resume, or `null` for a fresh session.
`soul` is a path, or `""` to inject nothing — the empty string is meaningful,
exactly as in v1, and it is how the native-session invariant is enforced from the
server side.

The reply is the session's real capability surface, which is only knowable once
the CLI has loaded the project:

```json
{"t":"ok","id":"o1","sessionId":"abc-123",
 "commands":[{"name":"verify","description":"Verify that a change does what it should",
              "argHint":"[target]","source":"skill"}],
 "subagents":[{"name":"Explore","description":"..."}],
 "models":[{"id":"opus-5","label":"Opus 5"}],
 "permissionMode":"ask","contextWindow":1000000}
```

`commands` is the point of the redesign in one field. On a real project this
returns dozens of entries — skills, project commands, plugins — where v1's
`--describe` returned one or two hand-written ones. The server does not curate
it; it is the agent's own answer to "what can I be asked to do".

Because commands change while a session runs (a skill is edited, a plugin
loads), the adapter re-sends the list as a `capabilities` event rather than the
server polling.

## Commands the server sends

| `t` | Meaning |
| --- | --- |
| `send` | A user turn: `text`, optional `attachments[]`, optional `command`+`args` |
| `interrupt` | Stop the current turn, keep the session |
| `steer` | Inject text mid-turn without ending it |
| `set` | Change `model`, `effort`, `permissionMode`, or `cwd` |
| `resolve` | Answer an adapter request, echoing its `id` |
| `query` | Ask for `context`, `cost`, `diff`, or `plan` |
| `close` | End the session cleanly; adapter flushes and exits |

`send` returns `ok` as soon as the turn is accepted, not when it finishes. Turn
completion is an event, because a turn outlives any request that started it —
the same reason `POST /turns` returns `202`.

## Requests the adapter sends

The inversion that v1 could not express: the agent asks, and a human answers.

```json
{"t":"request","id":"r4","kind":"approval","toolUseId":"toolu_01",
 "tool":"Bash","title":"Run a shell command",
 "detail":{"command":"rm -rf build","description":"Clean the build directory"},
 "options":[{"id":"allow","label":"Allow once"},
            {"id":"allow_always","label":"Allow Bash for this session"},
            {"id":"deny","label":"Deny"}],
 "default":"deny","risk":"destructive"}
```

The server journals it, publishes it to every attached client, and blocks
nothing else. A client answers:

```json
{"t":"resolve","id":"r4","option":"allow","input":{"command":"rm -rf build"}}
```

`input` may carry an edited version of what the agent proposed, which is how
"allow, but change the command" works — a real terminal affordance that a
yes/no dialog would drop.

Request kinds: `approval`, `dialog` (free-form question, e.g. Claude's
`request_user_dialog`), `elicitation` (MCP-originated structured input),
`choice`. Each is gated by `accepts` in the handshake.

**A pending request is durable state, not UI state.** It is journaled, survives a
client disconnect, and is re-delivered on reconnect — otherwise closing a laptop
lid strands an agent forever. If the link dies with requests outstanding, they
are journaled as `abandoned` so the timeline explains why the turn stopped
instead of showing an unexplained gap.

## Events the adapter sends

v1's event kinds survive unchanged — `thinking`, `assistant`, `tool_call`,
`tool_result`, `usage`, `status` with phases `started` / `delta` / `completed`,
where only `completed` is durable. v2 adds:

| `kind` | Carries |
| --- | --- |
| `subagent` | `parentToolUseId`, the child's own events nested |
| `plan` | A proposed plan, so plan mode is reviewable rather than prose |
| `diff` | A structured patch, so a file change renders as a diff not a tool log |
| `compact` | A compaction boundary, so the timeline shows where context was condensed |
| `turn` | `started` / `completed` with usage, cost, duration, stop reason |
| `capabilities` | Replaces the previous `commands`/`models`/`subagents` |
| `notice` | Agent-originated notification worth surfacing |

`subagent` is the one that changes what the UI can be. A subagent's trajectory
arrives tagged with its parent tool call, so the timeline can nest it — which is
what the terminal does, and what TraceDB already records as lineage. v1 saw only
an opaque `Task` tool call with a wall of text.

## Slash commands are text, not a side channel

A command is delivered as `send` with `command` set:

```json
{"t":"send","id":"c9","command":"verify","args":{"target":"the auth change"},"text":""}
```

The adapter is free to implement this as literal prompt text (`/verify ...`),
because for most CLIs that is precisely what a slash command is. Modelling it as
a *kind of turn* rather than a separate operation is what keeps it in the
transcript: a command produces normal events, appears in history, and is
searchable. v1's `--command` mode returned an ephemeral status line that was
shown once and lost.

Two consequences worth stating, because both were wrong in v1:

* A command's output is durable history, not a toast.
* A command can be interrupted, steered, and can raise approvals, because it is
  a turn like any other.

## Compatibility

The v1 contract stays supported, unchanged, for two reasons: the Discord daemon
runs on it today, and a v1 runner is the correct floor when a CLI genuinely has
no duplex mode.

The server picks a path per agent directory:

| Present | Path | Capability |
| --- | --- | --- |
| `adapter.sh` | v2 duplex | Full |
| `runner.sh` only | v1 one-shot | Turns, flags, declared commands; no approvals, interrupt, or live capability |

A v1 runner is presented as an adapter whose `supports` is the v1 subset, so
nothing above the seam branches on protocol version. The degradation surfaces as
capability, which the UI already reads — one code path, honest about what it can
do.

`SYLPH_STREAM_FORMAT=ndjson-v1` keeps its meaning for v1 runners. v2 adapters are
invoked as `adapter.sh --serve` and ignore it.

## What the adapter owns, and what it must not

Unchanged from v1 in principle, and worth restating because v2 gives the adapter
more rope:

The adapter owns vendor translation: how its CLI accepts a system prompt, spells
a resume id, streams output, and represents a permission request. It owns
injecting `SOUL.md` through an **additive** channel, never a replacing one.

The adapter owns no policy. It does not decide whether to inject identity (the
server sends `soul` or `""`), when a session resets, what a permission answer
should be, or what is worth journaling. A pending request must never be
auto-answered on the server's behalf — an adapter that helpfully approves a tool
call has silently removed the user from a decision the protocol exists to give
them.

## Failure

| Failure | Handling |
| --- | --- |
| Adapter exits mid-turn | Turn `failed`, stderr tail attached, pending requests `abandoned`, session detached |
| Adapter stops reading stdin | Command times out (30s), link torn down |
| Malformed line | Warned and skipped; a bad line is never fatal |
| Handshake timeout (10s) | Adapter marked broken, excluded from listings, reason recorded |
| Server restart | Every link is gone; sessions detached, in-flight turns `interrupted` |
| Request unanswered | Stays pending; it is real state, and only the user or a link teardown resolves it |

A link's death is never silent. The session records why it detached, because
"the agent stopped responding" and "the agent finished" look identical in a
transcript that does not say.



