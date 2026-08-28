#!/usr/bin/env python3
"""Scenario 04: the real session-config surface and its refusal shapes.

`config/value/write` turned out to be a config-FILE writer with no value-space
validation: it answered `status: ok` for `model_reasoning_effort = "banana"`
and for an entirely unknown key, and `config/read` echoed both back. So the
question "what is codex's typed config, and what does a refusal look like"
has to be asked of `thread/settings/update`, which is scoped to a thread.

Settles, with the reply captured for each:
  - does `thread/settings/update` validate a value, or accept it silently?
  - is an unknown option distinguishable from a bad value? (UnsupportedField
    vs InvalidValue -- docs/session-protocol.md needs both)
  - does the confirmation report what was applied, or echo the request?
    (AGENTS.md: record current_value from the vendor's confirmation)
  - what does a poisoned config.toml do to `thread/start`?
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import AppServer, jp

WORK = "/tmp/codex-probe-work"
os.makedirs(WORK, exist_ok=True)

s = AppServer("s04_settings")
s.initialize()

# Does a poisoned config.toml (model_reasoning_effort="banana",
# no_such_setting_at_all="x") break thread/start, or is it tolerated?
rid = s.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write"})
rep = s.wait_result(rid, timeout=60)
if "error" in rep:
    print("thread/start REFUSED with poisoned config:")
    print(json.dumps(rep, indent=1)[:900])
    sys.exit(0)
res = rep["result"]
tid = res["thread"]["id"]
print("thread:", tid)
print("effort reported by thread/start with a poisoned config:",
      json.dumps(res.get("reasoningEffort")))
print("model:", res.get("model"), " approvalPolicy:", res.get("approvalPolicy"))

print("\n===== configWarning / warning notifications so far =====")
for f in s.notifications():
    if f.get("method") in ("configWarning", "warning", "error", "deprecationNotice"):
        print(" ", f["method"], json.dumps(f.get("params"))[:400])

print("\n########## thread/settings/update ##########")
cases = [
    ("valid effort low", {"effort": "low"}),
    ("INVALID effort value", {"effort": "banana"}),
    ("valid approvalPolicy", {"approvalPolicy": "on-request"}),
    ("INVALID approvalPolicy", {"approvalPolicy": "sometimes"}),
    ("INVALID model", {"model": "no-such-model-xyz"}),
    ("unknown option name", {"noSuchOption": "x"}),
    ("valid personality", {"personality": "pragmatic"}),
    ("INVALID personality", {"personality": "grumpy"}),
    ("valid permissions profile", {"permissions": ":read-only"}),
    ("INVALID permissions profile", {"permissions": ":no-such-profile"}),
    ("effort ultra (declared by model/list)", {"effort": "ultra"}),
]
for label, patch in cases:
    params = dict(patch)
    params["threadId"] = tid
    rid = s.send("thread/settings/update", params)
    try:
        rep = s.wait_result(rid, timeout=30)
    except TimeoutError as e:
        print(f"\n--- {label}: TIMEOUT")
        continue
    print(f"\n--- {label}: sent {json.dumps(patch)} ---")
    print(json.dumps(rep, indent=1)[:1100])

print("\n===== thread/settings/updated notifications (the confirmations) =====")
for f in s.notifications("thread/settings/updated"):
    print(json.dumps(f.get("params"), indent=1)[:900])

# Read the thread back: does it report what we set, or what we asked for?
rid = s.send("thread/read", {"threadId": tid})
try:
    rep = s.wait_result(rid, timeout=30)
    print("\n===== thread/read after the sets =====")
    print(json.dumps(rep, indent=1)[:1600])
except Exception as e:
    print("thread/read:", e)

s.close()
print("\ncapture:", s.path)
