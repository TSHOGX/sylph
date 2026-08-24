#!/usr/bin/env python3
"""End-to-end check of the claude adapter against the real CLI.

Not a unit test: it spends real tokens and needs a working `claude` login, which
is why it lives beside the adapter and is run deliberately rather than in `cargo
test`. What it proves is the part no fake can — that the vendor translation is
right about a CLI we do not control.

    python3 adapters/claude/verify.py [workdir]

Checks, in order:
  1. `hello` reports the agent and its version.
  2. `open` returns the project's real slash commands (dozens, not two).
  3. A turn produces streamed assistant text and a completed turn event.
  4. An approval is raised, answered, and the turn continues past it.
"""
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
WORKDIR = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()

# An `ask` rule on Bash is what forces a real approval rather than hoping the
# ambient settings happen to withhold one.
SETTINGS = os.path.join(HERE, ".verify-settings.json")
with open(SETTINGS, "w") as handle:
    json.dump({"permissions": {"ask": ["Bash"], "allow": [], "deny": []}}, handle)

env = dict(os.environ)
env["SYLPH_SOUL"] = ""          # keep the check about protocol, not identity
env["SYLPH_CLAUDE_SETTINGS"] = SETTINGS

proc = subprocess.Popen(
    [os.path.join(HERE, "adapter.sh"), "--serve"],
    cwd=WORKDIR, env=env,
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, bufsize=1,
)

state = {
    "replies": {},
    "requests": [],
    "events": [],
    "turn_done": threading.Event(),
    "request_seen": threading.Event(),
}


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
        t = msg.get("t")
        if t in ("ok", "err"):
            state["replies"][msg.get("id")] = msg
        elif t == "request":
            state["requests"].append(msg)
            state["request_seen"].set()
        elif t == "event":
            state["events"].append(msg)
            if msg.get("kind") == "turn" and msg.get("phase") == "completed":
                state["turn_done"].set()


threading.Thread(target=reader, daemon=True).start()


def await_reply(request_id, timeout=45):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if request_id in state["replies"]:
            return state["replies"][request_id]
        time.sleep(0.1)
    raise SystemExit("FAIL: no reply to {} within {}s".format(request_id, timeout))


failures = []


def check(label, condition, detail=""):
    print(("  ok   " if condition else "  FAIL ") + label + (" — " + detail if detail else ""))
    if not condition:
        failures.append(label)


print("1. handshake")
send({"t": "hello", "id": "h1"})
hello = await_reply("h1")
check("reports the agent name", hello.get("agent", {}).get("name") == "claude")
check("reports a CLI version", bool(hello.get("agent", {}).get("version")),
      str(hello.get("agent", {}).get("version")))
check("declares approval support", "approval" in (hello.get("supports") or []))

print("2. open")
send({"t": "open", "id": "o1", "workdir": WORKDIR, "sessionId": None,
      "soul": "", "flags": {}, "permissionMode": "default", "env": {}})
opened = await_reply("o1", timeout=90)
commands = opened.get("commands") or []
check("returns a native session id", bool(opened.get("sessionId")),
      str(opened.get("sessionId")))
# The claim the redesign rests on. A v1 descriptor carries one or two.
check("returns the project's real command list", len(commands) > 5,
      "{} commands".format(len(commands)))
if commands:
    print("       e.g. " + ", ".join(c["name"] for c in commands[:6]))
check("returns subagent definitions", len(opened.get("subagents") or []) > 0,
      "{} subagents".format(len(opened.get("subagents") or [])))

# v3: the command list distinguishes what to ask the agent from what the session is.
control = [c for c in commands if c.get("kind") == "control"]
check("classifies control commands", len(control) > 0,
      ", ".join("/" + c["name"] for c in control))
check("a control command names its config option",
      all(c.get("configId") or c["name"] in ("context", "compact") for c in control))
check("carries the aliases the terminal honours",
      any(c.get("aliases") for c in commands),
      next((", ".join(c["aliases"]) for c in commands if c.get("aliases")), ""))

# The model catalogue, which v2 reported as empty on every session: it read `id`
# where the CLI answers `value`, and a wrong key fails silently.
config = opened.get("config") or []
models = next((o for o in config if o["id"] == "model"), None)
check("reports a non-empty model catalogue",
      bool(models and models.get("options")),
      "{} models".format(len(models.get("options") or []) if models else 0))
check("reports the current model rather than guessing",
      bool(models and models.get("currentValue")),
      str(models.get("currentValue") if models else None))

print("3. a turn streams and completes")
send({"t": "send", "id": "c1", "text": "Reply with exactly: SYLPH-OK"})
await_reply("c1")
if not state["turn_done"].wait(timeout=120):
    check("turn completed", False, "no turn/completed event in 120s")
else:
    kinds = [e.get("kind") for e in state["events"]]
    check("turn completed", True)
    check("streamed assistant text", "assistant" in kinds, "kinds seen: {}".format(set(kinds)))

print("4. an approval is raised and answered")
state["turn_done"].clear()
send({"t": "send", "id": "c2",
      "text": "Use the Bash tool to run: echo sylph-approval-probe"})
await_reply("c2")
if not state["request_seen"].wait(timeout=120):
    check("an approval was raised", False,
          "no request arrived; ambient permission settings may already allow Bash")
else:
    request = state["requests"][0]
    check("an approval was raised", request.get("kind") == "approval",
          "tool={}".format(request.get("tool")))
    check("carries the proposed input", request.get("detail") is not None,
          json.dumps(request.get("detail"))[:80])
    check("offers options including deny",
          any(o["id"] == "deny" for o in request.get("options") or []))

    # Answer it. This is the round trip v1 could not do at all.
    send({"t": "resolve", "id": request["id"], "option": "allow",
          "input": request.get("detail")})
    check("the turn continues past the approval",
          state["turn_done"].wait(timeout=120))

print("6. capability facets survive a turn")
# The regression v3 exists for. `claude` emits `system/init` once per *turn*, and in
# v2 the adapter turned each into a whole-surface update carrying only the permission
# mode — erasing all 46 commands on the first exchange.
partials = [e for e in state["events"]
            if e.get("kind") == "capabilities"]
check("a per-turn capability update names only the facet it changes",
      all(set((e.get("facets") or {}).keys()) <= {"permissionMode"} for e in partials),
      "{} update(s)".format(len(partials)))
check("no update carries a whole surface",
      all("capabilities" not in e for e in partials))

print("7. a typed config change answers with the refreshed facet")
send({"t": "set_config", "id": "s1", "configId": "model", "value": "sonnet"})
set_reply = await_reply("s1", timeout=30)
new_config = (set_reply.get("facets") or {}).get("config") or []
new_model = next((o for o in new_config if o["id"] == "model"), None)
check("the model is now what was asked for",
      bool(new_model and new_model.get("currentValue") == "sonnet"),
      str(new_model.get("currentValue") if new_model else None))

send({"t": "close", "id": "z1"})
try:
    proc.wait(timeout=15)
except subprocess.TimeoutExpired:
    proc.kill()

os.path.exists(SETTINGS) and os.remove(SETTINGS)

print()
if failures:
    print("FAILED: " + ", ".join(failures))
    sys.exit(1)
print("all checks passed")
