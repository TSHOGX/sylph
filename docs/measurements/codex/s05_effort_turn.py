#!/usr/bin/env python3
"""Scenario 05: does an accepted-but-bogus effort actually run?

`thread/settings/update` accepted `effort: "banana"`, answered `{}`, and
confirmed `effort: "banana"` in `thread/settings/updated`. That is the exact
shape AGENTS.md warns about -- but the confirmation echoing it back is only
half the question. The other half is whether the *turn* then fails, because
that decides whether an adapter can treat the confirmation as authoritative
or has to validate against `model/list` before sending.
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import AppServer

WORK = "/tmp/codex-probe-work"
os.makedirs(WORK, exist_ok=True)

s = AppServer("s05_effort_turn")
s.initialize()

rid = s.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write"})
res = s.wait_result(rid, timeout=60)["result"]
tid = res["thread"]["id"]
print("thread:", tid, "| clean-config effort:", json.dumps(res.get("reasoningEffort")))

rid = s.send("thread/settings/update", {"threadId": tid, "effort": "banana"})
print("settings/update(effort=banana) ->", json.dumps(s.wait_result(rid, timeout=30)))

rid = s.send("turn/start", {"threadId": tid,
                            "input": [{"type": "text", "text": "reply with the word ok"}]})
print("turn/start ->", json.dumps(s.wait_result(rid, timeout=30))[:300])

t0 = time.time()
done = False
while time.time() - t0 < 120 and not done:
    time.sleep(0.1)
    for f in s.notifications():
        m = f.get("method")
        if m in ("turn/completed", "error"):
            print(f"\n##### {m}")
            print(json.dumps(f.get("params"), indent=1)[:1500])
            done = True

print("\n===== all notifications =====")
for f in s.notifications():
    print(" ", f.get("method"))

# Now the same with a bogus MODEL, which also answered {}
rid = s.send("thread/settings/update", {"threadId": tid, "effort": "low",
                                        "model": "no-such-model-xyz"})
print("\nsettings/update(model=no-such-model-xyz) ->",
      json.dumps(s.wait_result(rid, timeout=30))[:300])
before = len(s.notifications())
rid = s.send("turn/start", {"threadId": tid,
                            "input": [{"type": "text", "text": "reply with the word ok"}]})
try:
    print("turn/start ->", json.dumps(s.wait_result(rid, timeout=60))[:400])
except Exception as e:
    print("turn/start:", e)

t0 = time.time()
while time.time() - t0 < 120:
    time.sleep(0.1)
    fs = s.notifications()
    hit = [f for f in fs[before:] if f.get("method") in ("turn/completed", "error")]
    if hit:
        for f in hit:
            print(f"\n##### {f['method']} (bogus model)")
            print(json.dumps(f.get("params"), indent=1)[:1500])
        break

s.close()
print("\ncapture:", s.path)
