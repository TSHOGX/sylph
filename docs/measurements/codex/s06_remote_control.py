#!/usr/bin/env python3
"""Scenario 06: the remote-control / pairing surface.

`docs/environments.md` § Pairing says Sylph's own pairing flow is modelled on
codex's, so what codex actually does is evidence about Sylph's design. And
`docs/agents.md` claims "remote-control start|stop|pair with short-lived
pairing codes" -- this measures the shape of those codes, what the pairing
grant covers, and whether a paired second party gets the *same* method surface
or a reduced one.

Nothing here connects to the user's real daemon: CODEX_HOME is a throwaway,
which is the `cc-socks` discipline from AGENTS.md applied to codex.
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import AppServer

s = AppServer("s06_remote_control")
s.initialize()

for method, params in [
    ("remoteControl/status/read", {}),
    ("remoteControl/client/list", {}),
]:
    rid = s.send(method, params)
    try:
        print(f"\n--- {method} ---")
        print(json.dumps(s.wait_result(rid, timeout=30), indent=1)[:1200])
    except Exception as e:
        print(f"--- {method}: {e}")

# Enable remote control, then start a pairing.
rid = s.send("remoteControl/enable", {})
try:
    print("\n--- remoteControl/enable ---")
    print(json.dumps(s.wait_result(rid, timeout=60), indent=1)[:1500])
except Exception as e:
    print("remoteControl/enable:", e)

rid = s.send("remoteControl/pairing/start", {})
pairing = None
try:
    rep = s.wait_result(rid, timeout=60)
    print("\n--- remoteControl/pairing/start ---")
    # A pairing code is credential-shaped. Print the SHAPE, mask the value.
    red = json.loads(json.dumps(rep))
    def mask(o):
        if isinstance(o, dict):
            return {k: ("<redacted len=%d>" % len(v) if isinstance(v, str) and k.lower() in
                        ("code", "token", "secret", "pairingcode", "bearer", "authtoken")
                        else mask(v)) for k, v in o.items()}
        if isinstance(o, list):
            return [mask(x) for x in o]
        return o
    print(json.dumps(mask(red), indent=1)[:1500])
    pairing = rep.get("result")
except Exception as e:
    print("remoteControl/pairing/start:", e)

rid = s.send("remoteControl/pairing/status", {})
try:
    print("\n--- remoteControl/pairing/status ---")
    rep = s.wait_result(rid, timeout=30)
    print(json.dumps(rep, indent=1)[:1200])
except Exception as e:
    print("remoteControl/pairing/status:", e)

rid = s.send("remoteControl/status/read", {})
try:
    print("\n--- remoteControl/status/read (after enable) ---")
    print(json.dumps(s.wait_result(rid, timeout=30), indent=1)[:1200])
except Exception as e:
    print("status/read:", e)

# What does the daemon expose, and where?
rid = s.send("server/diagnostics", {})
try:
    print("\n--- server/diagnostics ---")
    print(json.dumps(s.wait_result(rid, timeout=30), indent=1)[:2000])
except Exception as e:
    print("server/diagnostics:", e)

print("\n===== remoteControl notifications =====")
for f in s.notifications():
    if "remoteControl" in (f.get("method") or ""):
        print(" ", f["method"], json.dumps(f.get("params"))[:400])

# Leave the throwaway daemon disabled again.
rid = s.send("remoteControl/disable", {})
try:
    print("\n--- remoteControl/disable ---", json.dumps(s.wait_result(rid, timeout=30))[:400])
except Exception as e:
    print("disable:", e)

s.close()
print("\ncapture:", s.path)
