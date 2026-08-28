#!/usr/bin/env python3
"""Scenario 11: `codex exec --json` -- the other mode an adapter could use.

`docs/agents.md` says nothing about what `exec --json` emits, and it matters:
`exec` is the simplest possible `run` implementation (no daemon, no socket, no
websocket handshake), so if its event stream is equivalent to the app-server's
then the adapter has a much cheaper path.

Settles: what the JSONL envelopes are, whether the thread id is reported and
where, and whether the vocabulary matches the app-server's `item/*` shapes or
is a second, different one an adapter would have to translate separately.

Also measures `exec resume` and `exec fork` from the CLI side, since the
capability matrix claims both.
"""

import json
import os
import subprocess
import sys
import time
from collections import Counter

HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "captures")
os.makedirs(OUT, exist_ok=True)
os.makedirs(WORK, exist_ok=True)


def run(args, capture_name, timeout=600):
    env = dict(os.environ, CODEX_HOME=HOME)
    t0 = time.time()
    p = subprocess.Popen(["codex", *args], stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, env=env, text=True, cwd=WORK)
    frames = []
    path = os.path.join(OUT, capture_name + ".jsonl")
    with open(path, "w") as fh:
        for line in p.stdout:
            line = line.rstrip("\n")
            dt = time.time() - t0
            fh.write(json.dumps({"t": round(dt, 3), "raw": line}) + "\n")
            try:
                frames.append((dt, json.loads(line)))
            except json.JSONDecodeError:
                frames.append((dt, {"__unparsed__": line}))
    p.wait(timeout=timeout)
    err = p.stderr.read()
    return frames, err, path


print("########## codex exec --json ##########")
frames, err, path = run(
    ["exec", "--json", "--skip-git-repo-check", "-s", "workspace-write",
     "-C", WORK, "Run `echo hello-exec` then reply with the word ok."],
    "s11_exec_json")

print(f"frames: {len(frames)}  capture: {path}")
if err.strip():
    print("stderr (first 500):", err[:500])

print("\n===== envelope shapes, in order, with arrival times =====")
for dt, f in frames:
    if "__unparsed__" in f:
        print(f"  [{dt:6.2f}] RAW: {f['__unparsed__'][:120]}")
        continue
    keys = sorted(f.keys())
    kind = f.get("type") or f.get("method") or "?"
    extra = ""
    msg = f.get("msg")
    if isinstance(msg, dict):
        kind = f"msg.type={msg.get('type')}"
    item = f.get("item")
    if isinstance(item, dict):
        extra = f" item.type={item.get('type')}"
    print(f"  [{dt:6.2f}] keys={keys} {kind}{extra}")

print("\n===== distinct top-level shapes =====")
print(Counter(tuple(sorted(f.keys())) for _, f in frames if "__unparsed__" not in f))

print("\n===== the first envelope, verbatim (identity lives here?) =====")
for dt, f in frames[:3]:
    print(json.dumps(f, indent=1)[:1200])
    print("---")

print("\n===== any envelope carrying a thread/session id =====")
tid = None
for dt, f in frames:
    blob = json.dumps(f)
    for key in ("thread_id", "threadId", "session_id", "sessionId", "conversationId"):
        if f"\"{key}\"" in blob:
            print(f"  [{dt:6.2f}] {key} present:", json.dumps(f)[:400])
            try:
                found = json.loads(blob)
                def dig(o):
                    if isinstance(o, dict):
                        for k, v in o.items():
                            if k == key and isinstance(v, str):
                                return v
                            r = dig(v)
                            if r:
                                return r
                    if isinstance(o, list):
                        for v in o:
                            r = dig(v)
                            if r:
                                return r
                    return None
                tid = tid or dig(found)
            except Exception:
                pass
            break

print("\nthread id recovered from exec output:", tid)

# ---- exec resume ----
if tid:
    print("\n########## codex exec resume ##########")
    frames2, err2, path2 = run(
        ["exec", "--json", "--skip-git-repo-check", "-s", "workspace-write",
         "-C", WORK, "resume", tid, "reply with the word resumed"],
        "s11_exec_resume")
    print(f"frames: {len(frames2)}  capture: {path2}")
    if err2.strip():
        print("stderr:", err2[:400])
    ids = set()
    for _, f in frames2:
        b = json.dumps(f)
        if tid in b:
            ids.add("same-id")
    print("resume kept the same thread id:", "same-id" in ids)
    print("first envelope of resume:")
    if frames2:
        print(json.dumps(frames2[0][1], indent=1)[:800])

    # ---- exec fork ----
    print("\n########## codex exec fork ##########")
    frames3, err3, path3 = run(
        ["exec", "--json", "--skip-git-repo-check", "-s", "workspace-write",
         "-C", WORK, "fork", tid, "reply with the word forked"],
        "s11_exec_fork")
    print(f"frames: {len(frames3)}  capture: {path3}")
    if err3.strip():
        print("stderr:", err3[:400])
    if frames3:
        print("first envelope of fork:")
        print(json.dumps(frames3[0][1], indent=1)[:900])
    newid = None
    for _, f in frames3:
        def dig(o):
            if isinstance(o, dict):
                for k, v in o.items():
                    if k in ("thread_id", "threadId") and isinstance(v, str):
                        return v
                    r = dig(v)
                    if r:
                        return r
            if isinstance(o, list):
                for v in o:
                    r = dig(v)
                    if r:
                        return r
            return None
        newid = newid or dig(f)
    print("fork thread id:", newid, "| differs from origin:", newid != tid)
