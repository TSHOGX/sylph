#!/usr/bin/env python3
"""End-to-end check of the pi adapter against the real CLI.

Not a unit test: it spends real tokens and needs a working `pi` provider, which is
why it lives beside the adapter and is run deliberately rather than in `cargo test`.
What it proves is the part no fake can — that the vendor translation is right about a
CLI we do not control.

    python3 adapters/pi/verify.py [workdir]

The interesting checks are the ones where pi *differs* from claude, because those are
what the protocol had to generalize over:

  * It declares **no** `approval` capability. pi ships no sandbox and has no
    tool-permission callback, so an approval prompt could never arrive. Declaring it
    would promise a dialog that never appears, which is exactly the mismatch the
    handshake exists to prevent.
  * Its capability surface is *queried*, not pushed — three round trips at open.
  * Its thinking levels are per-model: the selected model here offers two of the
    seven pi documents, so a session-global effort list would be wrong.
"""
import json
import os
import subprocess
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
WORKDIR = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()

env = dict(os.environ)
env["SYLPH_SOUL"] = ""          # keep the check about protocol, not identity

proc = subprocess.Popen(
    [os.path.join(HERE, "adapter.sh"), "--serve"],
    cwd=WORKDIR, env=env,
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, bufsize=1,
)

state = {
    "replies": {},
    "events": [],
    "turn_done": threading.Event(),
}
failures = []


def send(obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def reader():
    for raw in proc.stdout:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except Exception:
            continue
        if msg.get("t") in ("ok", "err"):
            state["replies"][msg.get("id")] = msg
        elif msg.get("t") == "event":
            state["events"].append(msg)
            if msg.get("kind") == "turn" and msg.get("phase") == "completed":
                state["turn_done"].set()


threading.Thread(target=reader, daemon=True).start()


def await_reply(request_id, timeout=60):
    import time
    deadline = time.time() + timeout
    while time.time() < deadline:
        if request_id in state["replies"]:
            reply = state["replies"][request_id]
            if reply.get("t") == "err":
                fail("adapter refused {}".format(request_id), reply.get("message"))
                return {}
            return reply
        time.sleep(0.1)
    fail("no reply to {} within {}s".format(request_id, timeout))
    return {}


def check(label, ok, detail=""):
    print("  {}   {}{}".format("ok  " if ok else "FAIL", label,
                               " — " + str(detail) if detail else ""))
    if not ok:
        failures.append(label)


def fail(label, detail=""):
    check(label, False, detail)


print("1. handshake")
send({"t": "hello", "id": "h1", "protocol": "sylph.agent.v3",
      "accepts": ["approval", "dialog", "elicitation", "choice"]})
hello = await_reply("h1", timeout=30)
supports = hello.get("supports") or []
check("reports the agent name", hello.get("agent", {}).get("name") == "pi")
check("reports a CLI version", bool(hello.get("agent", {}).get("version")),
      str(hello.get("agent", {}).get("version")))
# The honest-degradation check, and the reason capability negotiation is not
# decorative: pi has no tool-permission callback, so a UI must not offer approvals.
check("declares NO approval support, because pi has no permission callback",
      "approval" not in supports, ", ".join(supports))
check("declares steer and interrupt",
      "steer" in supports and "interrupt" in supports)

print("2. open")
send({"t": "open", "id": "o1", "workdir": WORKDIR, "sessionId": None,
      "soul": "", "flags": {}, "env": {}})
opened = await_reply("o1", timeout=90)
check("returns a native session id", bool(opened.get("sessionId")),
      str(opened.get("sessionId"))[:12])

commands = opened.get("commands") or []
check("reports its command list", isinstance(commands, list),
      "{} commands".format(len(commands)))
# pi excludes its built-in TUI commands from `get_commands` by design, so there is
# nothing to classify — everything it reports is genuinely a prompt.
check("every reported command is a prompt",
      all(c.get("kind", "prompt") == "prompt" for c in commands))

config = opened.get("config") or []
models = next((o for o in config if o["id"] == "model"), None)
check("reports a model catalogue", bool(models and models.get("options")),
      "{} models".format(len(models.get("options") or []) if models else 0))
check("reports the currently selected model",
      bool(models and models.get("currentValue")),
      str(models.get("currentValue") if models else None))

thinking = next((o for o in config if o["id"] == "thinking"), None)
# Per-model, not session-global: this is why config options are re-derived after a
# model change rather than cached once.
check("reports thinking levels scoped to the selected model",
      bool(thinking and thinking.get("options")),
      ", ".join(o["value"] for o in (thinking.get("options") or [])) if thinking else "")

print("3. a turn streams and completes")
send({"t": "send", "id": "c1", "text": "Reply with exactly: SYLPH-OK"})
await_reply("c1")
check("turn completed", state["turn_done"].wait(timeout=120))
kinds = {e.get("kind") for e in state["events"]}
# Only the completed blocks. Summing deltas *and* completions would double every
# reply, since the deltas concatenate to exactly the completed text — which is the
# invariant `fold` relies on server-side.
text = "".join(e.get("text") or "" for e in state["events"]
               if e.get("kind") == "assistant" and e.get("phase") == "completed")
check("streamed assistant text", "assistant" in kinds, "kinds seen: {}".format(kinds))
check("the reply is what was asked for", "SYLPH-OK" in text, text[:60])

print("4. a typed config change answers with the refreshed facet")
send({"t": "set_config", "id": "s1", "configId": "thinking", "value": "off"})
set_reply = await_reply("s1", timeout=30)
new_config = (set_reply.get("facets") or {}).get("config") or []
new_thinking = next((o for o in new_config if o["id"] == "thinking"), None)
check("thinking is now what was asked for",
      bool(new_thinking and new_thinking.get("currentValue") == "off"),
      str(new_thinking.get("currentValue") if new_thinking else None))

send({"t": "close", "id": "z1"})
await_reply("z1", timeout=15)
proc.terminate()

print()
if failures:
    print("{} check(s) failed: {}".format(len(failures), "; ".join(failures)))
    sys.exit(1)
print("all checks passed")
