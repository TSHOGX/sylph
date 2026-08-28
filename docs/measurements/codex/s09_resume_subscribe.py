#!/usr/bin/env python3
"""Scenario 09: does thread/resume subscribe a second client, once a rollout exists?

s08's `thread/resume` was refused with "no rollout found for thread id" --
because a thread with no completed turn has not been written to disk yet. That
is itself a finding (a fresh thread is unresumable), but it left the real
question open. Here A runs a turn FIRST so the rollout exists, and only then
does B resume.

This is the decisive test for `docs/architecture.md` § "Observation is
multi-party": if B still sees no `item/*` after a successful resume, then
codex's app-server cannot serve a second observer at all, and Sylph's `watch`
for codex must tail the rollout JSONL rather than use the socket.
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

SOCK_DIR = "/private/tmp/codex-probe-sock9"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "captures", "s09_resume_subscribe.jsonl")
os.makedirs(os.path.dirname(CAP), exist_ok=True)
os.makedirs(WORK, exist_ok=True)
cap = open(CAP, "w")
caplock = threading.Lock()


def log(who, d, obj):
    with caplock:
        cap.write(json.dumps({"who": who, "dir": d, "t": time.time(), "frame": obj}) + "\n")
        cap.flush()


class Client:
    def __init__(self, name):
        self.name = name
        self.ws = UnixWebSocket(SOCK)
        self.frames = []
        self.lock = threading.Lock()
        self.next_id = 1
        self.alive = True
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            while self.alive:
                raw = self.ws.recv()
                try:
                    obj = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                log(self.name, "in", obj)
                with self.lock:
                    self.frames.append(obj)
        except WSError:
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

    def wait(self, rid, timeout=120):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                for fr in self.frames:
                    if fr.get("id") == rid and ("result" in fr or "error" in fr):
                        return fr
            time.sleep(0.02)
        raise TimeoutError(f"{self.name}: no reply to {rid}")

    def nots(self, method=None, since=0):
        with self.lock:
            fs = [f for f in self.frames if "method" in f and "id" not in f]
        return [f for f in fs[since:] if method is None or f.get("method") == method]

    def n(self):
        with self.lock:
            return len([f for f in self.frames if "method" in f and "id" not in f])

    def initialize(self):
        rid = self.send("initialize", {
            "clientInfo": {"name": f"probe-{self.name}", "version": "0.0.1"},
            "capabilities": {"experimentalApi": True}})
        r = self.wait(rid, timeout=30)
        self.send("initialized", {}, notify=True)
        return r

    def run_turn(self, tid, text, timeout=180):
        before = len(self.nots("turn/completed"))
        rid = self.send("turn/start", {"threadId": tid,
                                       "input": [{"type": "text", "text": text}]})
        self.wait(rid, timeout=60)
        t0 = time.time()
        while time.time() - t0 < timeout:
            if len(self.nots("turn/completed")) > before:
                return
            time.sleep(0.1)
        print(f"  (warn: {self.name} turn did not complete in {timeout}s)")


shutil.rmtree(SOCK_DIR, ignore_errors=True)
env = dict(os.environ, CODEX_HOME=HOME)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s09-err.log", "w"), env=env)
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)

a = Client("A")
a.initialize()
rid = a.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write"})
res = a.wait(rid, timeout=60)["result"]
tid = res["thread"]["id"]
rollout = res["thread"]["path"]
print("A thread:", tid)
print("rollout path:", rollout)
print("rollout exists before any turn:", os.path.exists(rollout))

print("\nA runs turn 1 so the rollout is written...")
a.run_turn(tid, "reply with the word alpha")
print("rollout exists after turn 1:", os.path.exists(rollout))

# Now B connects and resumes.
b = Client("B")
b.initialize()
rid = b.send("thread/resume", {"threadId": tid})
rep = b.wait(rid, timeout=90)
if "error" in rep:
    print("\nB thread/resume REFUSED:", json.dumps(rep["error"])[:400])
    resumed = False
else:
    r = rep["result"]
    print("\nB thread/resume OK; result keys:", sorted(r.keys()))
    th = r.get("thread") or {}
    print("   thread.id:", th.get("id"), "| turns returned:", len(th.get("turns") or []))
    resumed = True

a_before, b_before = a.n(), b.n()

print("\nA runs turn 2. Does B observe it?")
a.run_turn(tid, "Run `echo hi` then reply with the word gamma")
time.sleep(2.5)

print("\n===== turn 2 delivery =====")
print("A got:", dict(Counter(f.get("method") for f in a.nots(since=a_before))))
print("B got:", dict(Counter(f.get("method") for f in b.nots(since=b_before))))
b_items = [(f["params"].get("item") or {}).get("type")
           for f in b.nots(since=b_before) if (f.get("method") or "").startswith("item/")]
print("B item types:", b_items)
for f in b.nots(since=b_before):
    if f.get("method") == "item/completed":
        it = f["params"]["item"]
        if it.get("type") == "agentMessage":
            print("B saw agentMessage text:", json.dumps(it.get("text"))[:120])

# Now: is B's subscription real? unsubscribe should say something different.
rid = b.send("thread/unsubscribe", {"threadId": tid})
try:
    print("\nB thread/unsubscribe ->", json.dumps(b.wait(rid, timeout=30))[:300])
except Exception as e:
    print("B unsubscribe:", e)

b_mid = b.n()
print("\nA runs turn 3 after B unsubscribed...")
a.run_turn(tid, "reply with the word delta")
time.sleep(2.5)
print("B got:", dict(Counter(f.get("method") for f in b.nots(since=b_mid))))

# And can B still interrupt/steer after unsubscribing? (control vs observation)
rid = a.send("turn/start", {"threadId": tid, "input": [
    {"type": "text", "text": "Run `sleep 25` then reply with the word epsilon."}]})
a.wait(rid, timeout=60)
turn_id = None
t0 = time.time()
while time.time() - t0 < 60 and turn_id is None:
    ts = a.nots("turn/started")
    if ts:
        turn_id = ts[-1]["params"]["turn"]["id"]
    time.sleep(0.05)
time.sleep(5)
rid = b.send("turn/interrupt", {"threadId": tid, "turnId": turn_id})
try:
    print("\nB turn/interrupt while NOT subscribed ->",
          json.dumps(b.wait(rid, timeout=30))[:300])
except Exception as e:
    print("B interrupt:", e)
time.sleep(4)
print("A's last turn statuses:", [f["params"]["turn"]["status"]
                                 for f in a.nots("turn/completed")])

a.alive = b.alive = False
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
