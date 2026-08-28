#!/usr/bin/env python3
"""Scenario 14: can an interactive approval reach a SECOND party?

Approval policy is config; being asked is a different thing, and
`docs/session-protocol.md` § Requests is the whole inversion Sylph depends on
for AFK. The app-server declares five approval-shaped server->client REQUESTS
(`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`,
`item/permissions/requestApproval`, `execCommandApproval`, `applyPatchApproval`)
plus `item/tool/requestUserInput` and `mcpServer/elicitation/request`.

A JSON-RPC request is addressed to ONE peer, so the question is which one gets
it when two clients are attached: the client that started the turn, the
subscriber, or both. If only the turn's originator is asked, then a Sylph
envagent that merely observes cannot answer an approval, and codex's `control`
facet must declare approvals unavailable for an observed session.

Runs with approvalPolicy `untrusted` so a shell command must be approved, and
records which client received the request plus the exact params and the reply
shape the vendor accepts.
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from miniws import UnixWebSocket, WSError

SOCK_DIR = "/private/tmp/codex-probe-sock14"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "captures", "s14_approval.jsonl")
os.makedirs(os.path.dirname(CAP), exist_ok=True)
cap = open(CAP, "w")
lk = threading.Lock()


def log(who, d, obj):
    with lk:
        cap.write(json.dumps({"who": who, "dir": d, "t": time.time(),
                              "frame": obj}) + "\n")
        cap.flush()


class Client:
    def __init__(self, name):
        self.name = name
        self.ws = UnixWebSocket(SOCK)
        self.frames = []
        self.lock = threading.Lock()
        self.next_id = 1
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            while True:
                obj = json.loads(self.ws.recv())
                log(self.name, "in", obj)
                with self.lock:
                    self.frames.append(obj)
        except Exception:
            pass

    def send(self, method, params=None, notify=False):
        fr = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            fr["params"] = params
        rid = None
        if not notify:
            rid = self.next_id
            self.next_id += 1
            fr["id"] = rid
        log(self.name, "out", fr)
        self.ws.send_text(json.dumps(fr))
        return rid

    def respond(self, rid, result):
        fr = {"jsonrpc": "2.0", "id": rid, "result": result}
        log(self.name, "out", fr)
        self.ws.send_text(json.dumps(fr))

    def wait(self, rid, timeout=120):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                for f in self.frames:
                    if f.get("id") == rid and ("result" in f or "error" in f):
                        return f
            time.sleep(0.02)
        raise TimeoutError(f"{self.name}: no reply to {rid}")

    def server_requests(self):
        with self.lock:
            return [f for f in self.frames if "method" in f and "id" in f]

    def nots(self, method=None):
        with self.lock:
            return [f for f in self.frames if "method" in f and "id" not in f
                    and (method is None or f["method"] == method)]

    def initialize(self):
        rid = self.send("initialize", {
            "clientInfo": {"name": f"probe-{self.name}", "version": "0.0.1"},
            "capabilities": {"experimentalApi": True}})
        r = self.wait(rid, timeout=30)
        self.send("initialized", {}, notify=True)
        return r


shutil.rmtree(SOCK_DIR, ignore_errors=True)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s14-err.log", "w"),
                       env=dict(os.environ, CODEX_HOME=HOME))
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)

a = Client("A")
a.initialize()

# untrusted => a shell command needs approval
rid = a.send("thread/start", {"cwd": WORK, "approvalPolicy": "untrusted",
                             "sandbox": "read-only"})
res = a.wait(rid, timeout=60)["result"]
tid = res["thread"]["id"]
print("thread:", tid, "| approvalPolicy:", json.dumps(res.get("approvalPolicy")))

# One turn so a rollout exists, so B can subscribe.
print("\nturn 1 (so the rollout exists and B can resume)...")
rid = a.send("turn/start", {"threadId": tid,
                            "input": [{"type": "text", "text": "reply with the word ready"}]})
a.wait(rid, timeout=60)
t0 = time.time()
while time.time() - t0 < 180 and not a.nots("turn/completed"):
    time.sleep(0.1)

b = Client("B")
b.initialize()
rid = b.send("thread/resume", {"threadId": tid})
rep = b.wait(rid, timeout=90)
print("B thread/resume:", "OK" if "result" in rep else json.dumps(rep.get("error"))[:200])

a_reqs_before = len(a.server_requests())
b_reqs_before = len(b.server_requests())

print("\nturn 2: a command that must be approved...")
t0 = time.time()
rid = a.send("turn/start", {"threadId": tid, "input": [{"type": "text", "text":
             "Run the shell command `echo needs-approval`. Then reply with the word done."}]})
a.wait(rid, timeout=60)

# Watch BOTH clients for an approval request. Deliberately do not answer for
# 20s, so we can see whether the other client is asked too / whether it expires.
asked = {}
deadline = time.time() + 90
while time.time() < deadline:
    for who, c, base in (("A", a, a_reqs_before), ("B", b, b_reqs_before)):
        for f in c.server_requests()[base:]:
            key = (who, f["id"])
            if key in asked:
                continue
            asked[key] = f
            print(f"\n[t={time.time()-t0:5.1f}] {who} RECEIVED SERVER REQUEST "
                  f"{f.get('method')} id={f['id']}")
            print("   params:", json.dumps(f.get("params"), indent=1)[:1400])
    if asked:
        break
    time.sleep(0.1)

if not asked:
    print("\nno approval request arrived within 90s")
else:
    print(f"\n===== who was asked: {sorted(set(w for w, _ in asked))} =====")
    time.sleep(5)
    later = [(w, i) for (w, i) in asked]
    print("after a further 5s, requests seen by A:",
          len(a.server_requests()) - a_reqs_before,
          "| by B:", len(b.server_requests()) - b_reqs_before)

    # Answer from the client that was NOT asked first, if two were asked;
    # otherwise answer from the one that was.
    who, rid_req = sorted(asked)[0]
    target = a if who == "A" else b
    req = asked[(who, rid_req)]
    print(f"\nanswering from {who} with decision=approved")
    target.respond(rid_req, {"decision": "approved"})

    # Try answering the SAME request from the other client too.
    other = b if who == "A" else a
    print(f"attempting a second answer from {'B' if who=='A' else 'A'} "
          f"(same request id) ...")
    other.respond(rid_req, {"decision": "denied"})

    t1 = time.time()
    while time.time() - t1 < 180 and len(a.nots("turn/completed")) < 2:
        time.sleep(0.1)
    tc = a.nots("turn/completed")
    if len(tc) >= 2:
        print("\nturn 2 outcome:", json.dumps(tc[-1]["params"]["turn"]["status"]))
    for f in a.nots("item/completed"):
        it = f["params"]["item"]
        if it.get("type") == "commandExecution":
            print("command:", json.dumps(it.get("command")),
                  "| status:", it.get("status"),
                  "| output:", json.dumps(it.get("aggregatedOutput"))[:80])

print("\n===== serverRequest/resolved notifications =====")
for who, c in (("A", a), ("B", b)):
    for f in c.nots("serverRequest/resolved"):
        print(f"  {who}:", json.dumps(f.get("params"))[:300])

a.ws.close()
b.ws.close()
srv.terminate()
try:
    srv.wait(timeout=5)
except Exception:
    srv.kill()
cap.close()
shutil.rmtree(SOCK_DIR, ignore_errors=True)
print("\ncapture:", CAP)
