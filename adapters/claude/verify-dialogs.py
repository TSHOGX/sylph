#!/usr/bin/env python3
"""Does an answer to an AskUserQuestion actually reach the model?

Two things this proves, and the second is the one that was missing.

**A dialog arrives at all.** The CLI's schema says it "treats ABSENCE as 'cannot
display' and fails closed", so without `supportedDialogKinds` declared a
dialog-gated flow degrades to prose — which is how a slash command once came back
as a paragraph where a TUI shows a picker.

**The answer is received.** This is what the previous version of this script could
not see. It asserted that a request arrived and that answering unblocked the turn,
both of which were true while the answer itself was being written into
`questions[0].selectedOptions` — a key that occurs nowhere in the CLI binary. The
CLI read `answers[question]`, found nothing, and told the model "The user did not
answer the questions". Every layer behaved correctly and the pick was silently lost,
so a green run here reported a working feature to a user for whom it did not work.

The fix is to assert on the tool result the model actually reads: it has to quote the
picked label back, and the note with it.

    python3 adapters/claude/verify-dialogs.py [workdir]

Spends real tokens.
"""
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
WORKDIR = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()

NOTE = "only for generated files"

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
         "request_seen": threading.Event(), "results": []}


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
            # The tool result is where the answer lands: it is the text the model
            # reads, so it is the only place a lost answer is visible.
            if msg.get("kind") == "tool_result":
                state["results"].append(msg.get("text") or "")
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

    # The payoff: the agent's own options, declared as a form the client can render
    # rather than as buttons with nowhere to type.
    form = request.get("form") or {}
    fields = form.get("fields") or []
    check("it declares a form", len(fields) >= 1,
          "fields={}".format([f.get("id") for f in fields]))

    first = fields[0] if fields else {}
    choices = first.get("choices") or []
    check("the form carries the agent's own choices", len(choices) >= 2,
          str([c.get("value") for c in choices]))
    # Without this the user cannot answer anything the agent did not think of, which
    # the CLI's own dialog always allows.
    check("an answer outside the offered list is allowed",
          first.get("allowOther") is True)
    # A note is how "this option, but only under these circumstances" is expressed.
    check("a note may be attached to the answer", first.get("notes") is not None)
    check("its options are verbs rather than answers",
          {o.get("id") for o in request.get("options") or []} == {"submit", "deny"},
          str([o.get("id") for o in request.get("options") or []]))

    picked = (choices[0] or {}).get("value") if choices else None
    print("answering with {!r} plus a note".format(picked))
    send({"t": "resolve", "id": request["id"], "option": "submit",
          "input": {"values": {first.get("id"): picked},
                    "notes": {first.get("id"): NOTE}}})

    check("the turn continues after answering",
          state["turn_done"].wait(timeout=180))

    # The assertion the old script lacked. "The user did not answer the questions"
    # is what the CLI says when the answer never reached `answers[question]`, and
    # everything else about the round trip looks identical either way.
    joined = "\n".join(state["results"])
    check("the answer reached the model",
          picked is not None and picked in joined,
          "tool results: {!r}".format(joined[:400]))
    check("the note reached the model", NOTE in joined,
          "tool results: {!r}".format(joined[:400]))
    check("the model was not told the question went unanswered",
          "did not answer" not in joined)

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
