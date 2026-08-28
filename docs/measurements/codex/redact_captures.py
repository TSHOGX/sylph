#!/usr/bin/env python3
"""Redact the capture corpus before it is committed.

Three classes of content must not land in the repo, per the task's own caution
and AGENTS.md's rule about secret-shaped files:

  - `installationId` / `installation_id`: a stable per-install identifier that
    `remoteControl/status/changed` reports on every connection.
  - the model provider's `base_url` and any host from the user's config, since
    this install points at a third-party endpoint.
  - `base_instructions`: not secret, but it is the agent's entire system
    prompt repeated per session_meta, and it is 95% of the corpus by bytes.
    Replaced with a length marker so the SHAPE is still testable.

Redaction is by key, not by regex over the whole blob, so a value that merely
looks like a token is left alone and a real one cannot survive by reformatting.
"""

import glob
import json
import os
import re
import sys

CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)), "captures")

REDACT_KEYS = {
    "installationId", "installation_id",
    "accountId", "account_id", "chatgptAccountId",
    "authToken", "auth_token", "bearerToken", "token", "apiKey", "api_key",
    "pairingCode", "manualPairingCode", "code", "secret",
    # `serverName` is the machine's hostname. Sylph's own invariant is that an
    # environment id is never a hostname (AGENTS.md § Product Invariants), so a
    # fixture corpus should not carry one either.
    "serverName", "server_name",
}
ELIDE_KEYS = {"base_instructions", "baseInstructions", "developer_instructions"}
HOST_RE = re.compile(r"https?://[^\s\"']+")


def scrub(o):
    if isinstance(o, dict):
        out = {}
        for k, v in o.items():
            if k in REDACT_KEYS and isinstance(v, str):
                out[k] = f"<redacted:{k} len={len(v)}>"
            elif k in ELIDE_KEYS:
                if isinstance(v, str):
                    out[k] = f"<elided len={len(v)}>"
                elif isinstance(v, dict) and isinstance(v.get("text"), str):
                    out[k] = {"text": f"<elided len={len(v['text'])}>"}
                else:
                    out[k] = "<elided>"
            else:
                out[k] = scrub(v)
        return out
    if isinstance(o, list):
        return [scrub(x) for x in o]
    if isinstance(o, str):
        # Keep the shape of a URL but not the third-party host.
        return HOST_RE.sub(lambda m: "<url elided>", o) if "http" in o else o
    return o


total_before = total_after = 0
for path in sorted(glob.glob(os.path.join(CAP, "*.jsonl"))):
    before = os.path.getsize(path)
    total_before += before
    lines_out = []
    with open(path, errors="replace") as fh:
        for ln in fh:
            ln = ln.strip()
            if not ln:
                continue
            try:
                obj = json.loads(ln)
            except json.JSONDecodeError:
                # A raw non-JSON capture line (server log). Scrub textually.
                lines_out.append(json.dumps({"raw": HOST_RE.sub("<url elided>", ln)}))
                continue
            lines_out.append(json.dumps(scrub(obj)))
    with open(path, "w") as fh:
        fh.write("\n".join(lines_out) + "\n")
    after = os.path.getsize(path)
    total_after += after
    print(f"{os.path.basename(path):36s} {before:>9d} -> {after:>8d}")

print(f"\ntotal {total_before} -> {total_after} bytes")
