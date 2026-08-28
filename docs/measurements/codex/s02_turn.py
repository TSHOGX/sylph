#!/usr/bin/env python3
"""Scenario 02: one real turn over the app-server, captured frame by frame.

Settles questions 2 (event shapes), 3 (where the id is reported), and 6
(mid-turn flush): a cheap prompt, every frame recorded in arrival order with
timestamps, so the report can quote the envelope rather than describe it.

The prompt deliberately asks for a tool call as well as text, because
`item/started` / `item/completed` are generic over item type and the only way
to see which `item.type` values a turn really produces is to provoke them.
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import AppServer, jp

WORK = "/tmp/codex-probe-work"
os.makedirs(WORK, exist_ok=True)

s = AppServer("s02_turn")
s.initialize()

# thread/start takes NO threadId -- there is no field for one. The id comes back.
rid = s.send("thread/start", {
    "cwd": WORK,
    "approvalPolicy": "never",
    "sandbox": "workspace-write",
})
started = s.wait_result(rid, timeout=60)
jp("thread/start result", started)

thread_id = None
res = started.get("result") or {}
for key in ("threadId", "thread_id", "id"):
    if key in res:
        thread_id = res[key]
        print(f"\n>>> thread id reported under result.{key} = {thread_id}")
        break
if thread_id is None and isinstance(res.get("thread"), dict):
    thread_id = res["thread"].get("id")
    print(f"\n>>> thread id reported under result.thread.id = {thread_id}")

print("\n----- notifications so far -----")
for f in s.notifications():
    print(" ", f.get("method"))

# Now one cheap turn that also forces a tool call.
t0 = time.time()
rid = s.send("turn/start", {
    "threadId": thread_id,
    "input": [{"type": "text", "text":
               "Run the shell command `echo ok` and then reply with just the word done."}],
})
print(f"\n>>> turn/start id={rid} at t=0")

# Answer any server->client request (approvals) so the turn can finish.
seen = 0
deadline = time.time() + 240
turn_done = False
while time.time() < deadline and not turn_done:
    time.sleep(0.05)
    frames = s.notifications()
    with s.lock:
        allf = list(s.frames)
    for i, f in enumerate(allf):
        if i < seen:
            continue
        seen = i + 1
        m = f.get("method")
        if m and "id" in f:
            # server request -- must be answered or the turn blocks
            print(f"  [t={time.time()-t0:6.2f}] SERVER REQUEST {m} id={f['id']}")
            print("     params:", json.dumps(f.get("params"))[:600])
            if "Approval" in m or "approval" in m:
                s.respond(f["id"], {"decision": "approved"})
            elif m == "currentTime/read":
                s.respond(f["id"], {"currentTime": time.strftime("%Y-%m-%dT%H:%M:%SZ")})
            else:
                s.respond(f["id"], {})
        elif m:
            print(f"  [t={time.time()-t0:6.2f}] {m}")
            if m == "turn/completed":
                turn_done = True
        elif "result" in f or "error" in f:
            print(f"  [t={time.time()-t0:6.2f}] reply to id={f.get('id')}")

print("\n===== every distinct notification method, with counts =====")
from collections import Counter
c = Counter(f.get("method") for f in s.notifications())
for m, n in c.most_common():
    print(f"  {m}: {n}")

print("\n===== item/started and item/completed payloads (verbatim) =====")
for f in s.notifications():
    if f.get("method") in ("item/started", "item/completed"):
        print(f"\n--- {f['method']} ---")
        print(json.dumps(f.get("params"), indent=1)[:1800])

print("\n===== turn/started, turn/completed, thread/tokenUsage/updated =====")
for f in s.notifications():
    if f.get("method") in ("turn/started", "turn/completed",
                           "thread/tokenUsage/updated", "thread/status/changed",
                           "thread/started"):
        print(f"\n--- {f['method']} ---")
        print(json.dumps(f.get("params"), indent=1)[:1500])

s.close()
print("\nthread_id:", thread_id)
print("capture:", s.path)
