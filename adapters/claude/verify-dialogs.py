#!/usr/bin/env python3
"""Does declaring `supportedDialogKinds` actually produce a dialog?

The bug this checks for was reported from real use: sending a slash command
returned prose where a TUI shows a picker. The CLI's own schema explains why —
"treats ABSENCE as 'cannot display' and fails closed" — so the fix is a
declaration, and this proves the declaration works rather than assuming it.

    python3 adapters/claude/verify-dialogs.py [workdir]

Spends real tokens. Asks Claude to use AskUserQuestion, which is the cheapest
dialog to trigger deliberately, and asserts a `choice` request arrives carrying
the agent's own options.
"""
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
WORKDIR = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()

env = dict(os.environ)
env["SYLPH_SOUL"] = ""

proc = subprocess.Popen(
    [os.path.join(HERE, "adapter.sh"), "--serve"],
    cwd=WORKDIR, env=env,
    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    stderr=open('/tmp/adapter-stderr.log', 'w'),
    text=True, bufsize=1,
)

state = {"replies": {}, "requests": [], "turn_done": threading.Event(),
         "request_seen": threading.Event()}


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
        elif msg.get("t") == "request":
            state["requests"].append(msg)
            state["request_seen"].set()
        elif msg.get("t") == "event":
            if msg.get("kind") == "turn" and msg.get("phase") == "completed":
                state["turn_done"].set()


threading.Thread(target=reader, daemon=True).start()


def await_reply(request_id, timeout=90):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if request_id in state["replies"]:
            return state["replies"][request_id]
        time.sleep(0.1)
    raise SystemExit("FAIL: no reply to {}".format(request_id))


failures = []


def check(label, condition, detail=""):
    print(("  ok   " if condition else "  FAIL ") + label + (" - " + detail if detail else ""))
    if not condition:
        failures.append(label)


send({"t": "hello", "id": "h1"})
await_reply("h1")
send({"t": "open", "id": "o1", "workdir": WORKDIR, "sessionId": None,
      "soul": "", "flags": {}, "permissionMode": "default", "env": {}})
await_reply("o1", timeout=120)
print("session open")

print("asking for a question with options")
send({"t": "send", "id": "c1",
      "text": "Use the AskUserQuestion tool to ask me whether I prefer "
              "tabs or spaces. Offer exactly those two options."})
await_reply("c1")

if not state["request_seen"].wait(timeout=180):
    check("a dialog arrived", False,
          "no request; the CLI still thinks this client cannot render one")
else:
    # Claude may ask permission to *use* AskUserQuestion before asking the
    # question itself, so the first request is often a plain tool approval. Answer
    # anything that is not the choice we are looking for, and keep waiting.
    request = None
    deadline = time.time() + 180
    answered = set()
    while time.time() < deadline:
        for pending in list(state["requests"]):
            if pending["id"] in answered:
                continue
            if pending.get("kind") == "choice":
                request = pending
                break
            answered.add(pending["id"])
            print("   (answering a {} for {} first)".format(
                pending.get("kind"), pending.get("tool")))
            send({"t": "resolve", "id": pending["id"], "option": "allow"})
        if request or state["turn_done"].is_set():
            break
        time.sleep(0.5)

    if request is None:
        check("a choice dialog arrived", False,
              "kinds seen: {}".format([r.get("kind") for r in state["requests"]]))
        request = state["requests"][-1]
    check("a dialog arrived", True, "kind={}".format(request.get("kind")))
    # The payoff: the agent's own options, not a paragraph of prose.
    labels = [o.get("label") for o in request.get("options") or []]
    check("it carries the agent's options", len([l for l in labels if l]) >= 2,
          str(labels))
    check("it is answerable as a choice",
          any(str(o.get("id", "")).startswith("option:") for o in request.get("options") or []),
          str([o.get("id") for o in request.get("options") or []]))

    # Answer it and confirm the turn proceeds rather than parking.
    picked = next(
        (o["id"] for o in request["options"] if str(o["id"]).startswith("option:")),
        "cancel",
    )
    send({"t": "resolve", "id": request["id"], "option": picked})
    check("the turn continues after answering",
          state["turn_done"].wait(timeout=180))

send({"t": "close", "id": "z1"})
try:
    proc.wait(timeout=15)
except subprocess.TimeoutExpired:
    proc.kill()

print()
if failures:
    print("FAILED: " + ", ".join(failures))
    sys.exit(1)
print("all checks passed")
