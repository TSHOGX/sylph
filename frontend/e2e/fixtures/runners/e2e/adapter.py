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

        elif "question" in text:
            # A declared form, which is the shape an agent's own question takes: the
            # fields carry the answers and the options are verbs. Two fields, because
            # one could not observe that two radio groups stay independent — a single
            # shared `name` would make answering the second clear the first.
            #
            # `allowOther` and `notes` are both on the first field on purpose: they are
            # the two affordances whose absence was the reported bug, and neither has
            # any coverage unless a fixture declares them.
            emit({
                "t": "request", "id": "req-{}".format(self.turn), "kind": "choice",
                "tool": "AskUserQuestion",
                "title": "Two questions",
                "detail": {},
                "form": {"fields": [
                    {"type": "select", "id": "q:0", "label": "Indentation?",
                     "mode": "one", "allowOther": True,
                     "otherLabel": "Something else",
                     "notes": {"label": "Notes (optional)"},
                     "choices": [
                         {"value": "Tabs", "description": "Indent with tabs."},
                         {"value": "Spaces", "description": "Indent with spaces."},
                     ]},
                    {"type": "select", "id": "q:1", "label": "Linters?",
                     "mode": "many", "required": True,
                     "choices": [
                         {"value": "ESLint"},
                         {"value": "Prettier"},
                         {"value": "Clippy"},
                     ]},
                ]},
                "options": [
                    {"id": "submit", "label": "Send answer"},
                    {"id": "deny", "label": "Don't answer"},
                ],
                "default": "deny", "risk": "safe",
            })
            return

        elif "elicit" in text:
            # A text form, carrying a prefill. This is the kind whose entire purpose
            # is free text and which, before a form could be declared, rendered as a
            # read-only dump of its own placeholder.
            emit({
                "t": "request", "id": "req-{}".format(self.turn),
                "kind": "elicitation",
                "title": "Describe the change",
                "detail": {},
                "form": {"fields": [
                    {"type": "text", "id": "text", "label": "Summary",
                     "multiline": True, "required": True,
                     "prefill": "Refactor the ", "placeholder": "One sentence"},
                ]},
                "options": [
                    {"id": "submit", "label": "Submit"},
                    {"id": "cancel", "label": "Cancel"},
                ],
                "default": "cancel",
            })
            return

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

    def _tool(self, stream_id, name, tool_input, result, is_error=False, data=None):
        event("tool_call", "completed", streamId=stream_id, name=name,
              text=json.dumps(tool_input), data={"input": tool_input})
        # The exit code rides on the *result*, which is where the timeline reads it:
        # "failed" alone is not what anyone wants from a failed command.
        event("tool_result", "completed", streamId=stream_id, name=name,
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
            #
            # The received `input` is echoed into the result verbatim, so a spec can
            # assert what the *agent* got rather than only that the panel closed. That
            # distinction is the whole lesson of the bug this scenario exists for: the
            # panel closed, the turn resumed, and the answer had been dropped.
            option = msg.get("option")
            if option in ("deny", "cancel"):
                text = "Declined by the user."
            else:
                text = "Received: {}".format(
                    json.dumps(msg.get("input"), sort_keys=True))
            event("tool_result", "completed", streamId="blk-approved", name="Bash",
                  text=text, isError=option == "deny" or None)
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
