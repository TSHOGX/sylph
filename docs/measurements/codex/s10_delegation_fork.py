#!/usr/bin/env python3
"""Scenario 10: live delegation, and fork/resume identity.

Two things at once because both need a real multi-turn thread.

Delegation (question 5). The store shows `spawn_agent` function calls whose
output carries `{"agent_id":...,"nickname":...}`, and child rollouts carry
`source.subagent.thread_spawn.parent_thread_id`. What is unmeasured is the
LIVE shape: the item union declares `subAgentActivity` (agentThreadId,
agentPath, kind: started|interacted|interrupted|completed) and
`collabAgentToolCall` (senderThreadId, receiverThreadIds, agentsStates). This
provokes a real delegation and records which of those actually arrive, and
crucially whether the delegate's INTERIOR is forwarded -- the codex analogue of
`--forward-subagent-text` only forwarding for `Task`.

Fork/identity (question 3). `thread/fork` then checking whether the fork gets
a new id, what it records about its origin, and whether both remain usable.
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

SOCK_DIR = "/private/tmp/codex-probe-sock10"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "captures", "s10_delegation_fork.jsonl")
os.makedirs(os.path.dirname(CAP), exist_ok=True)
os.makedirs(WORK, exist_ok=True)
cap = open(CAP, "w")
lk = threading.Lock()


def log(who, d, obj):
    with lk:
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
                for f in self.frames:
                    if f.get("id") == rid and ("result" in f or "error" in f):
                        return f
            time.sleep(0.02)
        raise TimeoutError(f"{self.name}: no reply to {rid}")

    def nots(self, method=None, since=0):
        with self.lock:
            fs = [f for f in self.frames if "method" in f and "id" not in f]
        return [f for f in fs[since:] if method is None or f.get("method") == method]

    def reqs(self):
        """Server->client requests (they carry both method and id)."""
        with self.lock:
            return [f for f in self.frames if "method" in f and "id" in f]

    def n(self):
        with self.lock:
            return len([f for f in self.frames if "method" in f and "id" not in f])

    def respond(self, rid, result):
        fr = {"jsonrpc": "2.0", "id": rid, "result": result}
        log(self.name, "out", fr)
        self.ws.send_text(json.dumps(fr))

    def initialize(self):
        rid = self.send("initialize", {
            "clientInfo": {"name": f"probe-{self.name}", "version": "0.0.1"},
            "capabilities": {"experimentalApi": True}})
        r = self.wait(rid, timeout=30)
        self.send("initialized", {}, notify=True)
        return r

    def turn(self, tid, text, timeout=600, answer_requests=True):
        before = len(self.nots("turn/completed"))
        seen_reqs = set()
        rid = self.send("turn/start", {"threadId": tid,
                                       "input": [{"type": "text", "text": text}]})
        self.wait(rid, timeout=60)
        t0 = time.time()
        while time.time() - t0 < timeout:
            if answer_requests:
                for f in self.reqs():
                    if f["id"] in seen_reqs:
                        continue
                    seen_reqs.add(f["id"])
                    m = f.get("method", "")
                    print(f"   [server request] {m}")
                    if "pproval" in m:
                        self.respond(f["id"], {"decision": "approved"})
                    elif m == "currentTime/read":
                        self.respond(f["id"], {
                            "currentTime": time.strftime("%Y-%m-%dT%H:%M:%SZ")})
                    else:
                        self.respond(f["id"], {})
            if len(self.nots("turn/completed")) > before:
                return
            time.sleep(0.1)
        print(f"   (warn: turn did not complete in {timeout}s)")


shutil.rmtree(SOCK_DIR, ignore_errors=True)
env = dict(os.environ, CODEX_HOME=HOME)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s10-err.log", "w"), env=env)
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)

a = Client("A")
a.initialize()

# multiAgentMode: proactive makes delegation likely; the tool is spawn_agent.
rid = a.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write",
                             "multiAgentMode": "proactive"})
res = a.wait(rid, timeout=60)["result"]
tid = res["thread"]["id"]
print("thread:", tid, "| multiAgentMode:", res.get("multiAgentMode"))
print("rollout:", res["thread"]["path"])

before = a.n()
print("\n=== provoking a delegation ===")
a.turn(tid, "Delegate to a subagent: ask it to run `echo from-subagent` and "
            "report the output. Use your spawn_agent tool. Then tell me what it said.")

new = a.nots(since=before)
print("\nnotification tally:", dict(Counter(f.get("method") for f in new)))
print("item types:", Counter((f["params"].get("item") or {}).get("type")
                             for f in new if (f.get("method") or "").startswith("item/")))

print("\n===== every delegation-shaped item, verbatim =====")
for f in new:
    m = f.get("method") or ""
    if not m.startswith("item/"):
        continue
    it = f["params"].get("item") or {}
    if it.get("type") in ("subAgentActivity", "collabAgentToolCall"):
        print(f"\n--- {m} :: {it['type']} ---")
        print(json.dumps(f["params"], indent=1)[:1800])

# Any mcpToolCall / dynamicToolCall naming spawn_agent?
print("\n===== tool calls that mention agent spawning =====")
for f in new:
    m = f.get("method") or ""
    if not m.startswith("item/"):
        continue
    it = f["params"].get("item") or {}
    blob = json.dumps(it)
    if "spawn" in blob or "agent_id" in blob or "subagent" in blob.lower():
        print(f"\n--- {m} :: {it.get('type')} ---")
        print(json.dumps(it, indent=1)[:1400])

# What does the rollout say about the delegation?
print("\n===== rollout: spawn_agent records =====")
rp = res["thread"]["path"]
if os.path.exists(rp):
    for ln in open(rp, errors="replace"):
        try:
            o = json.loads(ln)
        except json.JSONDecodeError:
            continue
        p = o.get("payload") or {}
        if isinstance(p, dict) and (p.get("name") == "spawn_agent"
                                    or "agent_id" in json.dumps(p)[:2000]):
            print(json.dumps(o, indent=1)[:1200])

# Did a child rollout appear, and what does its session_meta say?
print("\n===== child rollouts in the throwaway home =====")
import glob
kids = []
for f2 in glob.glob(os.path.join(HOME, "sessions", "**", "rollout-*.jsonl"),
                    recursive=True):
    try:
        o = json.loads(open(f2, errors="replace").readline())
    except Exception:
        continue
    pay = o.get("payload") or {}
    src = pay.get("source")
    if isinstance(src, dict) and "subagent" in src:
        kids.append((f2, pay))
print(f"child rollouts found: {len(kids)}")
for f2, pay in kids[:2]:
    slim = {k: v for k, v in pay.items() if k != "base_instructions"}
    print(f"\n--- {os.path.basename(f2)} session_meta (base_instructions elided) ---")
    print(json.dumps(slim, indent=1)[:1400])

# ---------- fork ----------
print("\n\n########## thread/fork ##########")
rid = a.send("thread/fork", {"threadId": tid})
rep = a.wait(rid, timeout=90)
if "error" in rep:
    print("thread/fork REFUSED:", json.dumps(rep["error"])[:400])
else:
    r = rep["result"]
    th = r.get("thread") or {}
    print("fork result keys:", sorted(r.keys()))
    print("  new id      :", th.get("id"))
    print("  origin id   :", tid)
    print("  forkedFromId:", th.get("forkedFromId"))
    print("  parentThread:", th.get("parentThreadId"))
    print("  threadSource:", th.get("threadSource"))
    print("  path        :", th.get("path"))
    print("  turns copied:", len(th.get("turns") or []))
    fork_id = th.get("id")

    # Is the origin still usable after a fork?
    print("\norigin still runnable after fork?")
    a.turn(tid, "reply with the word origin-alive")
    print("  origin turn statuses:", [f["params"]["turn"]["status"]
                                     for f in a.nots("turn/completed")][-1:])

a.alive = False
a.ws.close()
srv.terminate()
try:
    srv.wait(timeout=5)
except Exception:
    srv.kill()
cap.close()
shutil.rmtree(SOCK_DIR, ignore_errors=True)
print("\ncapture:", CAP)
