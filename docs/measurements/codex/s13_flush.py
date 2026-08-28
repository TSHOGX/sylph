#!/usr/bin/env python3
"""Scenario 13: mid-turn flush granularity in the rollout file.

`docs/agents.md` claims "flushed at item completion, with `token_count` ticks
proving mid-turn append". That claim decides whether `watch` is offerable at
all (`docs/architecture.md`), so it needs a measurement rather than a repeat.

Method: run a turn with several distinguishable stages over the app-server,
polling the rollout file's size and line count ~10x/second, and record what
appears when. A file that only grows at the end cannot support `watch`; one
that grows per item can.

Also settles the shape of what a watcher would parse: the rollout's own
`type`/`payload` vocabulary, which is a THIRD spelling distinct from both the
app-server (`item/completed`, `commandExecution`) and `exec --json`
(`item.completed`, `command_execution`).
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from miniws import UnixWebSocket, WSError

SOCK_DIR = "/private/tmp/codex-probe-sock13"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "captures")
os.makedirs(OUT, exist_ok=True)

shutil.rmtree(SOCK_DIR, ignore_errors=True)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s13-err.log", "w"),
                       env=dict(os.environ, CODEX_HOME=HOME))
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)

frames = []
lock = threading.Lock()
ws = UnixWebSocket(SOCK)


def reader():
    try:
        while True:
            frames.append((time.time(), json.loads(ws.recv())))
    except Exception:
        pass


threading.Thread(target=reader, daemon=True).start()
nid = [0]


def call(method, params, timeout=120):
    nid[0] += 1
    rid = nid[0]
    ws.send_text(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method,
                             "params": params}))
    end = time.time() + timeout
    while time.time() < end:
        with lock:
            for _, f in frames:
                if f.get("id") == rid and ("result" in f or "error" in f):
                    return f
        time.sleep(0.02)
    raise TimeoutError(method)


def note_count(m):
    with lock:
        return len([1 for _, f in frames if f.get("method") == m and "id" not in f])


call("initialize", {"clientInfo": {"name": "probe", "version": "0.0.1"},
                    "capabilities": {"experimentalApi": True}})
ws.send_text(json.dumps({"jsonrpc": "2.0", "method": "initialized", "params": {}}))

r = call("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                          "sandbox": "workspace-write"})["result"]
tid = r["thread"]["id"]
rollout = r["thread"]["path"]
print("thread:", tid)
print("rollout:", rollout)
print("exists before turn:", os.path.exists(rollout))

samples = []          # (t, size, lines, last_payload_type)
stop = threading.Event()


def watch_file():
    t0 = time.time()
    last = (0, 0)
    while not stop.is_set():
        try:
            sz = os.path.getsize(rollout)
            with open(rollout, errors="replace") as fh:
                lines = fh.readlines()
            n = len(lines)
            if (sz, n) != last:
                last_type = None
                if lines:
                    try:
                        o = json.loads(lines[-1])
                        p = o.get("payload") or {}
                        last_type = f"{o.get('type')}/{p.get('type') if isinstance(p, dict) else '?'}"
                    except json.JSONDecodeError:
                        last_type = "partial-line"
                samples.append((round(time.time() - t0, 2), sz, n, last_type))
                last = (sz, n)
        except OSError:
            pass
        time.sleep(0.1)


threading.Thread(target=watch_file, daemon=True).start()

before = note_count("turn/completed")
t0 = time.time()
call("turn/start", {"threadId": tid, "input": [{"type": "text", "text":
     "Do these in order, one shell command each: run `echo STAGE-ONE`, "
     "then `sleep 3`, then `echo STAGE-TWO`, then `sleep 3`, then "
     "`echo STAGE-THREE`. Then reply with the word finished."}]})
while time.time() - t0 < 400 and note_count("turn/completed") == before:
    time.sleep(0.1)
turn_elapsed = time.time() - t0
time.sleep(2)
stop.set()
time.sleep(0.4)

print(f"\nturn took {turn_elapsed:.1f}s")
print(f"\n===== rollout growth during the turn ({len(samples)} distinct states) =====")
print(f"{'t(s)':>7} {'bytes':>8} {'lines':>6}  last record")
for t, sz, n, ty in samples:
    print(f"{t:7.2f} {sz:8d} {n:6d}  {ty}")

if samples:
    print(f"\nfirst append at t={samples[0][0]}s, last at t={samples[-1][0]}s")
    print("distinct append moments:", len(samples))
    print("VERDICT: the file grew mid-turn"
          if len(samples) > 2 and samples[0][0] < turn_elapsed - 1
          else "VERDICT: growth was not clearly mid-turn")

print("\n===== the rollout's own record vocabulary =====")
from collections import Counter
types = Counter()
with open(rollout, errors="replace") as fh:
    for ln in fh:
        try:
            o = json.loads(ln)
        except json.JSONDecodeError:
            continue
        p = o.get("payload")
        pt = p.get("type") if isinstance(p, dict) else None
        types[(o.get("type"), pt)] += 1
for k, v in types.most_common():
    print("  ", k, v)

print("\n===== token_count / usage records, verbatim =====")
shown = 0
with open(rollout, errors="replace") as fh:
    for ln in fh:
        try:
            o = json.loads(ln)
        except json.JSONDecodeError:
            continue
        blob = json.dumps(o)
        if "token" in blob.lower() and shown < 3:
            print(json.dumps(o, indent=1)[:900])
            shown += 1

shutil.copy(rollout, os.path.join(OUT, "s13_rollout_sample.jsonl"))
print("\nrollout copied to captures/s13_rollout_sample.jsonl")

ws.close()
srv.terminate()
try:
    srv.wait(timeout=5)
except Exception:
    srv.kill()
shutil.rmtree(SOCK_DIR, ignore_errors=True)
