#!/usr/bin/env python3
"""Deterministic E2E agent, speaking `sylph.agent.v3`.

Real agents are non-deterministic and need credentials, so the suite runs against
this instead and still exercises the production path end to end: a held-open duplex
link, a handshake that reports capabilities, streamed events, and requests the UI
has to answer.

This replaced a v1 `runner.sh`, and the replacement was not optional. Discovery
requires `adapter.sh`, so after the v1 path was deleted the fixture directory held
nothing discoverable and the backend reported `Agent discovery: 0 usable, 0 broken`
— every spec that created a session or ran a turn was failing against an agent that
did not exist. A fixture that cannot be discovered is worse than a missing one,
because the suite still runs and the failures look like product bugs.

Scenarios are selected by substring of the prompt, so one fixture serves the whole
suite. The `paced` one exists because every other scenario finishes faster than a
single render, which makes them pass even against a client that streams nothing —
the exact blind spot that let a fully non-streaming client ship once already.
"""
import json
import sys
import threading
import time

PROTOCOL = "sylph.agent.v3"

# Deliberately a mix of prompt and control commands: the UI's routing between "send
# this as a turn" and "open a picker" is a real behaviour, and a fixture that only
# reported prompts could not exercise it.
COMMANDS = [
    {"name": "status", "description": "Report runner status", "kind": "prompt",
     "source": "builtin"},
    {"name": "verify", "description": "Verify a change end to end", "kind": "prompt",
     "argHint": "[target]", "source": "skill"},
    {"name": "usage", "description": "Show session cost", "kind": "prompt",
     "source": "builtin", "aliases": ["cost", "stats"]},
    {"name": "model", "description": "Choose the model", "kind": "control",
     "configId": "model", "source": "builtin"},
    # A control command onto the long catalogue. The dialog a control command opens
    # reaches the filterable list through a different surface than the composer's
    # popover does — an `Overlay` that owns its own dismissal — so without this the
    # dialog's search path had no coverage at all.
    {"name": "provider", "description": "Choose the provider model", "kind": "control",
     "configId": "provider", "source": "builtin"},
]

# A long catalogue, because the threshold between "a menu" and "a searchable list"
# is a real behaviour and nothing could observe it while every fixture option had two
# choices. Fourteen is above `SELECT_SEARCH_THRESHOLD` and in the range real agents
# report — `pi` declares 21 across providers, `claude` around a dozen — and the ids
# are deliberately long enough to be the kind that once widened a `<select>` past the
# right edge of a phone. Descriptions carry the provider, so a spec can assert the
# filter matches the hint and not only the label.
CATALOGUE = [
    ("opus-5-1m", "Opus 5 (1M context)", "Anthropic"),
    ("opus-5", "Opus 5", "Anthropic"),
    ("sonnet-5", "Sonnet 5", "Anthropic"),
    ("haiku-4-5", "Haiku 4.5", "Anthropic"),
    ("fable-5", "Fable 5", "Anthropic"),
    ("gpt-5-codex-preview", "GPT-5 Codex Preview", "OpenAI"),
    ("gpt-5-mini", "GPT-5 Mini", "OpenAI"),
    ("o5-reasoning", "o5 Reasoning", "OpenAI"),
    ("gemini-3-pro", "Gemini 3 Pro", "Google"),
    ("gemini-3-flash", "Gemini 3 Flash", "Google"),
    ("llama-4-405b-instruct", "Llama 4 405B Instruct", "Meta"),
    ("mistral-large-3", "Mistral Large 3", "Mistral"),
    ("qwen-3-max", "Qwen 3 Max", "Alibaba"),
    ("deepseek-v4", "DeepSeek V4", "DeepSeek"),
    # Two providers offering the same model under the same display name, which is a
    # measured shape rather than an invented one: pi 0.84.3 offers two "Claude Opus 5"
    # (anthropic direct and via openrouter) and two each of "DeepSeek V4 Pro" and
    # "GLM-5.2". The reported bug was both rows showing a checkmark, because a
    # producer's `checked: value === currentValue` is a filter and the bare id was not
    # unique. Values here are provider-qualified for the same reason the pi adapter's
    # now are; the pair exists so a spec can prove exactly one row reads as current
    # and that each provider is separately reachable.
    ("ikun glm-5.2", "GLM-5.2", "ikun-glm"),
    ("huaweicloud glm-5.2", "GLM-5.2", "huaweicloud"),
    # Deliberately refused by `set_config` below, so the client's failure path is
    # observable rather than only reasoned about.
    ("refused", "Unavailable Model", "Refuses to be set"),
]

CONFIG = [
    # Two choices, so the menu pattern stays covered: below the threshold a config
    # option is a `menu` of `menuitemradio`s, and that is a different ARIA contract
    # from the searchable list below, not merely a different width.
    {"type": "select", "id": "model", "name": "Model", "category": "model",
     "currentValue": "fast",
     "options": [{"value": "fast", "name": "Fast"},
                 {"value": "thorough", "name": "Thorough"}]},
    # Above it, and in the same category so it lands on the composer's control row
    # beside the short one — which is what lets one screen assert both patterns.
    {"type": "select", "id": "provider", "name": "Provider model",
     "category": "model", "currentValue": "sonnet-5",
     "options": [{"value": value, "name": name, "description": provider}
                 for value, name, provider in CATALOGUE]},
    {"type": "boolean", "id": "verbose", "name": "Verbose", "category": "output",
     "currentValue": False},
]

out_lock = threading.Lock()


def emit(obj):
    with out_lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


def event(kind, phase=None, **fields):
    body = {"t": "event", "kind": kind}
    if phase:
        body["phase"] = phase
    body.update({k: v for k, v in fields.items() if v is not None})
    emit(body)


class Fixture:
    def __init__(self):
        self.config = [dict(option) for option in CONFIG]
        self.turn = 0

    def option(self, config_id):
        return next((o for o in self.config if o["id"] == config_id), None)

    def run_turn(self, text):
        """One turn's worth of events, then a terminal `turn` event.

        Runs on its own thread so the link stays readable while it emits: a turn that
        blocked the read loop could not be interrupted, and interrupt is one of the
        things the suite needs to test.
        """
        self.turn += 1
        event("status", "started", text="starting")
        # A per-turn capability update naming ONE facet, which is what `claude`
        # actually does: its `system/init` arrives once per turn and carries only the
        # permission mode. This line is why the suite can observe the regression at
        # all — a partial update erasing the facets it never mentioned is what stopped
        # the slash menu opening after the first exchange, and no spec could see it
        # while the fixture only ever reported its surface once, at open.
        event("capabilities", facets={"permissionMode": "default"})
        event("thinking", "completed", streamId="blk-think",
              text="Considering the request.")

        if "bulk" in text:
            # A long transcript, for measuring render cost. Completed blocks only, so
            # every one is journalled and survives a reload — which is what makes the
            # measurement reproducible rather than dependent on stream timing.
            for i in range(1, 251):
                event("assistant", "completed", streamId="blk-a{}".format(i),
                      text="Step {}: inspecting the module and its callers.".format(i))
                self._tool("blk-t{}".format(i), "Read",
                           {"file_path": "src/mod_{}.rs".format(i)},
                           "pub fn helper_{}() -> usize {{ {} }}".format(i, i))
                event("thinking", "completed", streamId="blk-th{}".format(i),
                      text="Module {} looks consistent with the others.".format(i))
            self._reply()

        elif "tools" in text:
            # A read, an edit carrying a real diff, and a failing command: the three
            # renderings whose expansion policy differs.
            self._tool("blk-read", "Read", {"file_path": "src/lib.rs"},
                       'fn main() {\n    println!("before");\n}')
            event("assistant", "completed", streamId="blk-plan",
                  text="I will replace the placeholder with **real** logging.")
            self._tool(
                "blk-edit", "Edit",
                {"file_path": "src/lib.rs",
                 "old_string": 'fn main() {\n    println!("before");\n}',
                 "new_string": 'fn main() {\n    tracing::info!("after");\n}'},
                "Applied 1 edit.")
            self._tool("blk-bash", "Bash", {"command": "cargo test --lib"},
                       "error[E0425]: cannot find value `after` in this scope",
                       is_error=True, data={"exitCode": 101})
            self._reply()

        elif "approval" in text:
            # The inversion v1 could not express, and the reason the fixture had to
            # become a duplex adapter at all: a request that blocks until answered.
            emit({
                "t": "request", "id": "req-{}".format(self.turn), "kind": "approval",
                "tool": "Bash", "toolUseId": "toolu_e2e",
                "title": "Run a shell command",
                "detail": {"command": "rm -rf build"},
                "options": [
                    {"id": "allow", "label": "Allow once"},
                    {"id": "allow_always", "label": "Always allow Bash",
                     "persistent": True},
                    {"id": "deny", "label": "Deny"},
                ],
                "default": "deny", "risk": "destructive",
            })
            # Left pending on purpose. The turn resumes from `resolve`, which is what
            # lets a spec assert that answering is what unblocks it.
            return

        elif "narrow" in text:
            # An approval that forbids a lasting grant. Offering "always" here would
            # write a rule broader than the ask, so a client must not show it.
            emit({
                "t": "request", "id": "req-{}".format(self.turn), "kind": "approval",
                "tool": "Write", "title": "Write to a sensitive path",
                "detail": {"file_path": "/etc/hosts"},
                "options": [
                    {"id": "allow", "label": "Allow once"},
                    {"id": "deny", "label": "Deny"},
                ],
                "default": "deny", "risk": "destructive",
                "allowPersistent": False,
            })
            return

        elif "delegate" in text:
            # A real delegation tree, because a shallow one cannot exhibit the
            # defects this shape exists to prevent. Concretely it needs: two
            # trajectories running *concurrently* (interleaved on one wire, which is
            # the only way to catch grouping by arrival order instead of by `span`),
            # one nested three deep (a flat subagent list passes at depth 1 and
            # breaks at depth 2), a failure buried at the bottom (a collapsed card
            # showing green over it is the worst outcome of collapsing), and one left
            # open (the server closes it, and nothing else tests that path).
            event("assistant", "completed", streamId="blk-dispatch",
                  text="Fanning out to two agents.")

            # Two open before either emits: their events then interleave.
            event("subagent", "started", streamId="sub-explore", name="Explore")
            event("subagent", "started", streamId="sub-verify", name="Verify")

            event("thinking", "completed", streamId="blk-ex-think",
                  span="sub-explore", text="Searching for the call sites.")
            event("thinking", "completed", streamId="blk-vf-think",
                  span="sub-verify", text="Planning the verification.")
            self._tool("blk-ex-grep", "Grep", {"pattern": "fn handler"},
                       "src/api.rs:42: fn handler()", span="sub-explore")

            # Depth 3: a trajectory inside a trajectory inside the turn. This is the
            # dynamic-workflow shape — a phase that itself delegates.
            event("subagent", "started", streamId="sub-worker",
                  span="sub-verify", name="Worker")
            self._tool("blk-wk-bash", "Bash", {"command": "cargo test --lib"},
                       "error[E0425]: cannot find value `after`",
                       is_error=True, data={"exitCode": 101}, span="sub-worker")
            event("subagent", "completed", streamId="sub-worker", span="sub-verify")

            # Closes clean, but its descendant failed: the card must still report a
            # failure, or collapsing hides it.
            event("subagent", "completed", streamId="sub-verify")
            event("subagent", "completed", streamId="sub-explore",
                  text="Found 3 call sites.")

            # Opened and never closed. The turn settling is what resolves it, so a
            # spec can assert it does not spin forever.
            event("subagent", "started", streamId="sub-stranded", name="Stranded")
            self._reply()

        elif "promote" in text:
            # Delegation that is never *declared* — the shape every delegating tool
            # outside claude's built-in `Task` actually has, and the one no fixture
            # covered. A host is discovered to be a container only because its
            # children name it (`docs/session-protocol.md` § Promotion), so nothing
            # here emits a `subagent` event for the hosts themselves.
            #
            # Sized deliberately. A host with *one* delegate cannot exhibit the
            # collapse this exists to catch: keyed by the host, N delegates become one
            # row where the last name wins, and with N=1 that is indistinguishable
            # from correct. Measured on a real session as a two-agent workflow
            # rendering as a single card, so the fixture runs two.
            event("assistant", "completed", streamId="pr-intro",
                  text="Running a workflow.")

            # Host 1: multiplexes two delegates, and forwards no interior of its own.
            # Its own close is its `tool_result`, not its call's `completed` phase —
            # a call completes when its arguments finish streaming, which says
            # nothing about the delegated work.
            event("tool_call", "completed", streamId="pr-host-a", name="Workflow",
                  text=json.dumps({"script": "parallel([agentOne, agentTwo])"}))
            event("subagent", "started", streamId="pr-task-1",
                  span="pr-host-a", name="Demo: agent-1")
            event("subagent", "started", streamId="pr-task-2",
                  span="pr-host-a", name="Demo: agent-2")
            # Progress as a *snapshot*: repeated, and identical each time. Appended as
            # delta text this is what stored one sentence six times.
            for _ in range(3):
                event("subagent", "delta", streamId="pr-task-1", span="pr-host-a",
                      data={"progress": "Working on the demo"})
            event("thinking", "completed", streamId="pr-t1-think",
                  span="pr-task-1", text="Reading the brief.")
            event("subagent", "completed", streamId="pr-task-1", span="pr-host-a",
                  text="agent-1 done")
            # A failure inside the second delegate, so a spec can assert the promoted
            # host reports it while collapsed.
            self._tool("pr-t2-bash", "Bash", {"command": "cargo test"},
                       "error: test failed", is_error=True,
                       data={"exitCode": 101}, span="pr-task-2")
            event("subagent", "completed", streamId="pr-task-2", span="pr-host-a",
                  text="agent-2 failed", is_error=True)
            event("tool_result", "completed", streamId="pr-host-a",
                  name="Workflow", text="workflow finished")

            # Host 2: reports that delegated work ran and forwards none of it. A
            # closed trajectory with no children is a terminal state, not a loading
            # one, and must not present as an expandable card that never fills.
            event("tool_call", "completed", streamId="pr-host-b", name="Agent",
                  text=json.dumps({"task": "summarize"}))
            event("subagent", "completed", streamId="pr-host-b",
                  text="reported nothing")

            # Host 3: promoted, and left open. Only the server closes this, so a spec
            # can assert a promoted host does not spin forever either — the path that
            # `phase = 'started'` used to miss entirely.
            event("tool_call", "completed", streamId="pr-host-c", name="Workflow",
                  text=json.dumps({"script": "phase('Stage A')"}))
            event("subagent", "started", streamId="pr-task-3",
                  span="pr-host-c", name="Stage A: A:item-1")
            self._reply()

        elif "paced" in text:
            # Deliberately slow, so "did the UI paint mid-turn" is observable rather
            # than inferred. Each stage is separated by far longer than a frame, so a
            # spec can assert an earlier stage is on screen while a later one has not
            # been emitted yet.
            event("thinking", "started", streamId="blk-slow-think")
            event("thinking", "delta", streamId="blk-slow-think",
                  text="Reasoning step one. ")
            time.sleep(1)
            event("thinking", "delta", streamId="blk-slow-think",
                  text="Reasoning step two.")
            event("thinking", "completed", streamId="blk-slow-think",
                  text="Reasoning step one. Reasoning step two.")
            time.sleep(1)
            self._tool("blk-slow-tool", "Read", {"file_path": "src/paced.rs"},
                       "pub fn paced() {}")
            time.sleep(1)
            # A second thinking block: multi-round reasoning has to stream, not only
            # the first block.
            event("thinking", "completed", streamId="blk-slow-think2",
                  text="Second round of reasoning.")
            time.sleep(1)
            self._reply(stream=True)

        else:
            self._reply(stream=True)

        self.settle()

    def _tool(self, stream_id, name, tool_input, result, is_error=False, data=None,
              span=None):
        event("tool_call", "completed", streamId=stream_id, name=name, span=span,
              text=json.dumps(tool_input), data={"input": tool_input})
        # The exit code rides on the *result*, which is where the timeline reads it:
        # "failed" alone is not what anyone wants from a failed command.
        #
        # The result carries the same `span` as its call: a delegate's tool output
        # belongs to the delegate, and rooting it would show a subagent's failure as
        # the parent's own.
        event("tool_result", "completed", streamId=stream_id, name=name, span=span,
              text=result, isError=is_error or None, data=data)

    def _reply(self, stream=False):
        if stream:
            event("assistant", "started", streamId="blk-reply")
            event("assistant", "delta", streamId="blk-reply", text="Hello from ")
            event("assistant", "delta", streamId="blk-reply", text="the E2E runner.")
        event("assistant", "completed", streamId="blk-reply",
              text="Hello from the E2E runner.")

    def settle(self):
        event("usage", "completed", streamId="usage",
              data={"inputTokens": 12, "outputTokens": 7})
        event("turn", "completed", text="Hello from the E2E runner.",
              data={"stopReason": "end_turn"})


def serve():
    fixture = Fixture()

    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except Exception:
            continue

        t = msg.get("t")
        mid = msg.get("id")

        if t == "hello":
            emit({"t": "ok", "id": mid, "adapter": "e2e/3.0.0",
                  "agent": {"name": "e2e", "version": "3.0.0"},
                  "supports": ["interrupt", "steer", "approval", "slash",
                               "set_config", "context"],
                  # The descriptor's spawn-time choices, which are a different list
                  # from the live `config` above and are what the settings panel's
                  # per-agent default reads. Long here for the same reason: that
                  # control is a second surface with the same scroll-and-filter
                  # problem, and a two-item list could not observe it.
                  "flags": {"model": {
                      "choices": ["fast", "thorough"]
                                 + [value for value, _, _ in CATALOGUE],
                      "description": "Model tier"}}})

        elif t == "open":
            # A stable id, so a resumed session in a spec is recognisably the same
            # conversation rather than a new one that happens to look alike.
            emit({"t": "ok", "id": mid,
                  "sessionId": msg.get("sessionId") or "e2e-native-session",
                  "commands": COMMANDS,
                  "subagents": [{"name": "Explore", "description": "Read-only search"}],
                  "config": fixture.config,
                  "permissionMode": "default",
                  "contextWindow": 200000})

        elif t == "send":
            for config_id, value in (msg.get("config") or {}).items():
                option = fixture.option(config_id)
                if option:
                    option["currentValue"] = value
            text = msg.get("text") or ""
            command = msg.get("command")
            if command:
                text = "/{} {}".format(
                    command,
                    " ".join(str(v) for v in (msg.get("args") or {}).values()),
                ).strip()
            emit({"t": "ok", "id": mid})
            threading.Thread(target=fixture.run_turn, args=(text,), daemon=True).start()

        elif t == "interrupt":
            emit({"t": "ok", "id": mid})
            event("turn", "completed", text="", data={"stopReason": "interrupted"})

        elif t == "steer":
            emit({"t": "ok", "id": mid})
            event("assistant", "completed", streamId="blk-steered",
                  text="Steered: {}".format(msg.get("text") or ""))

        elif t == "set_config":
            option = fixture.option(msg.get("configId"))
            if not option:
                emit({"t": "err", "id": mid, "code": "UNSUPPORTED_FIELD",
                      "message": "no such config option"})
                continue
            # A refusable value, so a spec can observe what a *rejected* set looks
            # like. This is not hypothetical: a real adapter sending the wrong field
            # name to its vendor CLI gets a failed RPC back, and with no error path in
            # the client the rejection was invisible — the value simply did not
            # change, which is indistinguishable from a broken control.
            if msg.get("value") == "refused":
                emit({"t": "err", "id": mid, "code": "UNSUPPORTED_FIELD",
                      "message": "this agent refuses that value"})
                continue
            option["currentValue"] = msg.get("value")
            # The whole facet, because setting one option may change another's
            # choices — the contract every adapter honours here.
            emit({"t": "ok", "id": mid, "facets": {"config": fixture.config}})

        elif t == "resolve":
            # The answer arrives, so the blocked turn can finish. A denial is still an
            # answer: the turn ends either way, which is what the timeline shows.
            option = msg.get("option")
            event("tool_result", "completed", streamId="blk-approved", name="Bash",
                  text="Denied by the user." if option == "deny" else "Command ran.",
                  isError=option == "deny" or None)
            fixture._reply()
            fixture.settle()

        elif t == "query":
            emit({"t": "ok", "id": mid,
                  "context": {"usedTokens": 1200, "contextWindow": 200000}})

        elif t == "close":
            emit({"t": "ok", "id": mid})
            return


if __name__ == "__main__":
    if "--serve" in sys.argv[1:]:
        serve()
    else:
        sys.stderr.write("usage: adapter.py --serve\n")
        sys.exit(2)
