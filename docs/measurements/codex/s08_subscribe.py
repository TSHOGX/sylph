#!/usr/bin/env python3
"""Scenario 08: can a second client SUBSCRIBE to a thread it did not start?

s07 measured an inversion of what `docs/architecture.md` assumes. On one
app-server daemon with two websocket clients:

  - CONTROL is multi-party: B interrupted A's running turn (`turn/interrupt`
    -> `{}`, and A saw `turn/completed` status `interrupted`), and B started a
    turn on A's thread successfully.
  - OBSERVATION is NOT multi-party: B received only `thread/started` and
    `thread/status/changed`. Zero `item/*`, zero `turn/started`, zero
    `turn/completed` -- including for the turn **B itself started**, whose
    agentMessage text went to A.

So event delivery is scoped to a subscriber, not broadcast, and the method
list has `thread/unsubscribe` with no matching subscribe. This measures which
call makes B a subscriber: `thread/resume` is the candidate, since it is how a
client attaches to an existing thread.

That question is load-bearing for Sylph: if a second party cannot observe,
`watch` over the socket is impossible and the envagent must fall back to
tailing the rollout JSONL.
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

SOCK_DIR = "/private/tmp/codex-probe-sock8"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "captures", "s08_subscribe.jsonl")
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

    def wait(self, rid, timeout=90):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                for fr in self.frames:
                    if fr.get("id") == rid and ("result" in fr or "error" in fr):
                        return fr
            time.sleep(0.02)
        raise TimeoutError(f"{self.name}: no reply to {rid}")

    def notifs(self, method=None, since=0):
        with self.lock:
            fs = [f for f in self.frames if "method" in f and "id" not in f]
        return [f for f in fs[since:] if method is None or f.get("method") == method]

    def count(self):
        with self.lock:
            return len([f for f in self.frames if "method" in f and "id" not in f])

    def initialize(self):
        rid = self.send("initialize", {
            "clientInfo": {"name": f"probe-{self.name}", "version": "0.0.1"},
            "capabilities": {"experimentalApi": True}})
        r = self.wait(rid, timeout=30)
        self.send("initialized", {}, notify=True)
        return r


shutil.rmtree(SOCK_DIR, ignore_errors=True)
env = dict(os.environ, CODEX_HOME=HOME)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s08-err.log", "w"), env=env)
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)

a, b = Client("A"), Client("B")
a.initialize()
b.initialize()

rid = a.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write"})
tid = a.wait(rid, timeout=60)["result"]["thread"]["id"]
print("A started thread:", tid)

# B attaches via thread/resume -- does that make B a subscriber?
rid = b.send("thread/resume", {"threadId": tid})
try:
    rep = b.wait(rid, timeout=60)
    if "error" in rep:
        print("\nB thread/resume REFUSED:", json.dumps(rep["error"])[:400])
    else:
        r = rep["result"]
        print("\nB thread/resume OK. keys:", sorted(r.keys()))
        print("   thread.id:", (r.get("thread") or {}).get("id"))
except Exception as e:
    print("B thread/resume:", e)

b_before = b.count()
a_before = a.count()

# A runs a turn. Does B now see the items?
t0 = time.time()
rid = a.send("turn/start", {"threadId": tid, "input": [
    {"type": "text", "text": "Run `echo hello` then reply with the word alpha."}]})
a.wait(rid, timeout=60)

done = False
while time.time() - t0 < 180 and not done:
    time.sleep(0.1)
    for f in a.notifs("turn/completed"):
        done = True

time.sleep(2)
print(f"\n===== after A's turn (t={time.time()-t0:.1f}) =====")
print("A new notifications:", dict(Counter(
    f.get("method") for f in a.notifs(since=a_before))))
print("B new notifications:", dict(Counter(
    f.get("method") for f in b.notifs(since=b_before))))

print("\nB item/* payload types seen:",
      [(f["params"].get("item") or {}).get("type")
       for f in b.notifs(since=b_before) if f.get("method", "").startswith("item/")])

# Did B see the assistant text?
for f in b.notifs(since=b_before):
    if f.get("method") == "item/completed":
        it = f["params"]["item"]
        if it.get("type") == "agentMessage":
            print("B saw agentMessage:", json.dumps(it.get("text"))[:120])

# Try thread/unsubscribe from B, then A runs another turn.
rid = b.send("thread/unsubscribe", {"threadId": tid})
try:
    print("\nB thread/unsubscribe ->", json.dumps(b.wait(rid, timeout=30))[:300])
except Exception as e:
    print("B unsubscribe:", e)

b_mid = b.count()
rid = a.send("turn/start", {"threadId": tid, "input": [
    {"type": "text", "text": "reply with the word beta"}]})
a.wait(rid, timeout=60)
t1 = time.time()
done = False
while time.time() - t1 < 180 and not done:
    time.sleep(0.1)
    if len(a.notifs("turn/completed")) >= 2:
        done = True
time.sleep(2)
print("\n===== after unsubscribe, A ran another turn =====")
print("B new notifications:", dict(Counter(
    f.get("method") for f in b.notifs(since=b_mid))))

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
