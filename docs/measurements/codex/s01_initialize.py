#!/usr/bin/env python3
"""Scenario 01: initialize, and enumerate what the server says it can do.

Settles: what `initialize` reports, the real model catalogue shape (the
`claude` instance of the vendor-key defect was reading `id` off a catalogue
that answers `value`/`displayName`), the collaboration/permission enumerations
that a Sylph `config` facet would be built from, and remote-control status.
"""

import json
import sys

sys.path.insert(0, __import__("os").path.dirname(__file__))
from probe import AppServer, jp

s = AppServer("s01_initialize")
rep = s.initialize()
jp("initialize result", rep)

for method, params in [
    ("model/list", {}),
    ("collaborationMode/list", {}),
    ("permissionProfile/list", {}),
    ("remoteControl/status/read", {}),
    ("configRequirements/read", {}),
    ("account/read", {}),
    ("experimentalFeature/list", {}),
]:
    try:
        rid = s.send(method, params)
        r = s.wait_result(rid, timeout=30)
        jp(method, r)
    except Exception as e:
        print(f"\n----- {method} FAILED: {e}")

print("\n----- notifications seen during initialize -----")
for f in s.notifications():
    print(" ", f.get("method"))

s.close()
print("\ncapture:", s.path)
