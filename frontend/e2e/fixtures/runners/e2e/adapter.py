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
]

CONFIG = [
    {"type": "select", "id": "model", "name": "Model", "category": "model",
     "currentValue": "fast",
     "options": [{"value": "fast", "name": "Fast"},
                 {"value": "thorough", "name": "Thorough"}]},
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
                  "flags": {"model": {"choices": ["fast", "thorough"],
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
