#!/usr/bin/env python3
"""Scenario 03: the control surface, against a live turn.

Settles question 1 and question 4. Four things, each with the reply captured:

  a. `turn/interrupt` on a running turn -- does codex support interrupt at all?
  b. `turn/steer` mid-turn -- can a second party redirect work in flight?
  c. `thread/queue/add` -- what `codex queue` is built on.
  d. `config/value/write` refusals -- AGENTS.md records that `claude` answers
     success while silently normalizing, so a refusal here is tested for
     rather than assumed. Both an unknown key and a bad value, because
     `docs/session-protocol.md` needs UnsupportedField distinguishable from
     InvalidValue.

The turn is deliberately slow (a sleep loop) so there is a window to act in.
`AGENTS.md` records that a runner faster than one render cannot observe
streaming; the same applies to observing an interrupt.
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import AppServer, jp

WORK = "/tmp/codex-probe-work"
os.makedirs(WORK, exist_ok=True)

s = AppServer("s03_control")
s.initialize()

rid = s.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write"})
res = s.wait_result(rid, timeout=60)["result"]
tid = res["thread"]["id"]
print("thread:", tid)
print("thread/start reported model=", res.get("model"),
      "effort=", res.get("reasoningEffort"),
      "approvalPolicy=", res.get("approvalPolicy"))

# ---------- (d) config refusals, before any turn ----------
print("\n########## config/value/write refusal behaviour ##########")
cases = [
    ("real key, real value", {"keyPath": "model_reasoning_effort", "value": "low",
                              "mergeStrategy": "replace"}),
    ("real key, BOGUS value", {"keyPath": "model_reasoning_effort", "value": "banana",
                               "mergeStrategy": "replace"}),
    ("BOGUS key", {"keyPath": "no_such_setting_at_all", "value": "x",
                   "mergeStrategy": "replace"}),
    ("real key, wrong type", {"keyPath": "model_reasoning_effort", "value": 42,
                              "mergeStrategy": "replace"}),
    ("approval policy via config", {"keyPath": "approval_policy", "value": "never",
                                    "mergeStrategy": "replace"}),
]
for label, params in cases:
    r = s.send("config/value/write", params)
    try:
        rep = s.wait_result(r, timeout=20)
        print(f"\n--- {label} ---")
        print(json.dumps(rep, indent=1)[:900])
    except TimeoutError as e:
        print(f"\n--- {label}: TIMEOUT {e}")

# What does config/read report back? (current values from the vendor,
# never from what we last set -- AGENTS.md rule.)
r = s.send("config/read", {})
try:
    rep = s.wait_result(r, timeout=20)
    print("\n--- config/read (truncated) ---")
    print(json.dumps(rep, indent=1)[:2500])
except Exception as e:
    print("config/read failed:", e)

# ---------- (a)(b)(c) live-turn control ----------
print("\n########## live turn: steer, then interrupt ##########")
t0 = time.time()
turn_rid = s.send("turn/start", {
    "threadId": tid,
    "input": [{"type": "text", "text":
               "Run `sleep 25` in the shell, then run `sleep 25` again, "
               "then reply with the word finished."}],
})

# Wait for turn/started so we have the turnId to act on.
_, tf = s.wait_notification("turn/started", timeout=60)
turn_id = tf["params"]["turn"]["id"]
print(f"[t={time.time()-t0:.2f}] turn started id={turn_id}")

# Let it get into the first sleep.
time.sleep(8)

# (c) queue a message while the turn runs
qr = s.send("thread/queue/add", {"threadId": tid,
                                 "input": [{"type": "text", "text": "queued note"}]})
try:
    print(f"\n--- thread/queue/add (t={time.time()-t0:.1f}) ---")
    print(json.dumps(s.wait_result(qr, timeout=20), indent=1)[:800])
except Exception as e:
    print("queue/add:", e)

qlr = s.send("thread/queue/list", {"threadId": tid})
try:
    print(f"\n--- thread/queue/list ---")
    print(json.dumps(s.wait_result(qlr, timeout=20), indent=1)[:900])
except Exception as e:
    print("queue/list:", e)

# (b) steer mid-turn
sr = s.send("turn/steer", {
    "threadId": tid, "expectedTurnId": turn_id,
    "input": [{"type": "text", "text": "Stop sleeping. Just say steered."}],
})
try:
    print(f"\n--- turn/steer (t={time.time()-t0:.1f}) ---")
    print(json.dumps(s.wait_result(sr, timeout=30), indent=1)[:900])
except Exception as e:
    print("turn/steer:", e)

# also try steering with a WRONG expectedTurnId, to see the refusal shape
sr2 = s.send("turn/steer", {
    "threadId": tid, "expectedTurnId": "00000000-0000-0000-0000-000000000000",
    "input": [{"type": "text", "text": "nope"}],
})
try:
    print(f"\n--- turn/steer with wrong expectedTurnId ---")
    print(json.dumps(s.wait_result(sr2, timeout=30), indent=1)[:900])
except Exception as e:
    print("turn/steer(wrong):", e)

time.sleep(10)

# (a) interrupt
ir = s.send("turn/interrupt", {"threadId": tid, "turnId": turn_id})
try:
    print(f"\n--- turn/interrupt (t={time.time()-t0:.1f}) ---")
    print(json.dumps(s.wait_result(ir, timeout=30), indent=1)[:900])
except Exception as e:
    print("turn/interrupt:", e)

# What did the turn end as?
try:
    _, cf = s.wait_notification("turn/completed", timeout=90)
    print(f"\n--- turn/completed after interrupt (t={time.time()-t0:.1f}) ---")
    print(json.dumps(cf["params"], indent=1)[:1600])
except Exception as e:
    print("no turn/completed:", e)

# interrupt again, now that nothing is running -- refusal shape for a dead turn
ir2 = s.send("turn/interrupt", {"threadId": tid, "turnId": turn_id})
try:
    print("\n--- turn/interrupt on an already-finished turn ---")
    print(json.dumps(s.wait_result(ir2, timeout=20), indent=1)[:700])
except Exception as e:
    print("second interrupt:", e)

print("\n===== notification sequence, in order =====")
for f in s.notifications():
    m = f.get("method")
    extra = ""
    p = f.get("params") or {}
    if m in ("item/started", "item/completed"):
        extra = " " + str((p.get("item") or {}).get("type"))
    if m == "thread/status/changed":
        extra = " " + json.dumps(p.get("status"))
    print(" ", m, extra)

s.close()
print("\ncapture:", s.path)
