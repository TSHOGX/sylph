#!/usr/bin/env python3
"""Scenario 15: an approval answered with the CORRECT decision token.

s14 established the routing (every subscriber is asked, `serverRequest/resolved`
retires it everywhere) but answered `{"decision": "approved"}` -- and
`approved` is not in the vocabulary. The real enum is
`accept` | `acceptForSession` | `decline` | `cancel` plus two object variants
(`acceptWithExecpolicyAmendment`, `applyNetworkPolicyAmendment`). The turn
still reported `completed` while the command reported `status: failed` with
null output, so a wrong decision token is NOT an error -- it reads as a denial.

That is the vendor-key defect class again, in the one place it costs the most:
an adapter that guesses the token has an approval channel that silently denies
everything, and the turn still looks successful.

This scenario proves the difference by running the same command twice, once
with `accept` and once with `decline`, and comparing the resulting item.
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from miniws import UnixWebSocket

SOCK_DIR = "/private/tmp/codex-probe-sock15"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "captures", "s15_approval_decisions.jsonl")
os.makedirs(os.path.dirname(CAP), exist_ok=True)
cap = open(CAP, "w")


def log(d, obj):
    cap.write(json.dumps({"dir": d, "t": time.time(), "frame": obj}) + "\n")
    cap.flush()


shutil.rmtree(SOCK_DIR, ignore_errors=True)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s15-err.log", "w"),
                       env=dict(os.environ, CODEX_HOME=HOME))
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)

ws = UnixWebSocket(SOCK)
frames = []
lock = threading.Lock()


def reader():
    try:
        while True:
            o = json.loads(ws.recv())
            log("in", o)
            with lock:
                frames.append(o)
    except Exception:
        pass


threading.Thread(target=reader, daemon=True).start()
nid = [0]


def send(method, params=None, notify=False):
    fr = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        fr["params"] = params
    rid = None
    if not notify:
        nid[0] += 1
        rid = nid[0]
        fr["id"] = rid
    log("out", fr)
    ws.send_text(json.dumps(fr))
    return rid


def respond(rid, result):
    fr = {"jsonrpc": "2.0", "id": rid, "result": result}
    log("out", fr)
    ws.send_text(json.dumps(fr))


def wait(rid, timeout=120):
    end = time.time() + timeout
    while time.time() < end:
        with lock:
            for f in frames:
                if f.get("id") == rid and ("result" in f or "error" in f):
                    return f
        time.sleep(0.02)
    raise TimeoutError(str(rid))


def nots(m=None):
    with lock:
        return [f for f in frames if "method" in f and "id" not in f
                and (m is None or f["method"] == m)]


def srv_reqs():
    with lock:
        return [f for f in frames if "method" in f and "id" in f]


send("initialize", {"clientInfo": {"name": "probe", "version": "0.0.1"},
                    "capabilities": {"experimentalApi": True}})
time.sleep(0.5)
send("initialized", {}, notify=True)

rid = send("thread/start", {"cwd": WORK, "approvalPolicy": "untrusted",
                           "sandbox": "read-only"})
tid = wait(rid, timeout=60)["result"]["thread"]["id"]
print("thread:", tid)


def turn_with_decision(prompt, decision, label):
    tc_before = len(nots("turn/completed"))
    req_before = len(srv_reqs())
    ic_before = len(nots("item/completed"))
    rid = send("turn/start", {"threadId": tid,
                              "input": [{"type": "text", "text": prompt}]})
    wait(rid, timeout=60)
    answered = False
    t0 = time.time()
    while time.time() - t0 < 240:
        if not answered:
            for f in srv_reqs()[req_before:]:
                print(f"  [{label}] asked: {f.get('method')} id={f['id']}")
                print(f"  [{label}] availableDecisions:",
                      json.dumps((f.get('params') or {}).get('availableDecisions')))
                respond(f["id"], {"decision": decision})
                print(f"  [{label}] answered with {json.dumps(decision)}")
                answered = True
                break
        if len(nots("turn/completed")) > tc_before:
            break
        time.sleep(0.1)
    time.sleep(1.5)
    tc = nots("turn/completed")
    print(f"  [{label}] turn status:",
          json.dumps(tc[-1]["params"]["turn"]["status"]) if tc else "none")
    for f in nots("item/completed")[ic_before:]:
        it = f["params"]["item"]
        if it.get("type") == "commandExecution":
            print(f"  [{label}] command status: {it.get('status')!r}"
                  f" exitCode={it.get('exitCode')!r}"
                  f" output={json.dumps(it.get('aggregatedOutput'))[:60]}")
    return answered


print("\n########## decision = 'accept' (the real token) ##########")
turn_with_decision("Run the shell command `echo ACCEPTED-OK`. Then reply with done.",
                   "accept", "accept")

print("\n########## decision = 'decline' ##########")
turn_with_decision("Run the shell command `echo SHOULD-NOT-RUN`. Then reply with done.",
                   "decline", "decline")

print("\n########## decision = 'approved' (NOT in the enum) ##########")
turn_with_decision("Run the shell command `echo BOGUS-TOKEN`. Then reply with done.",
                   "approved", "bogus")

ws.close()
srv.terminate()
try:
    srv.wait(timeout=5)
except Exception:
    srv.kill()
cap.close()
shutil.rmtree(SOCK_DIR, ignore_errors=True)
print("\ncapture:", CAP)
