#!/usr/bin/env python3
"""Scenario 12: does `exec resume <id>` keep the id, and does the app-server?

s11's resume check was too loose -- it tested whether the origin id appeared
anywhere in the output, and the origin id legitimately appears in resumed
history. But the `thread.started` envelope reported a DIFFERENT id, which if
real is the single most consequential fact for `docs/session-protocol.md`
§ Opening a session: Sylph keys a session on `(environment, agent, native_id)`,
so a resume that mints a new native id means the identity of a conversation is
not stable across resumes and every resume would look like a new session.

This measures it exactly: run one turn, capture the id, resume it explicitly,
and compare the ids reported by each. Then do the same over the app-server
(`thread/resume`), because the two modes may differ and the adapter uses both.
"""

import glob
import json
import os
import shutil
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from miniws import UnixWebSocket, WSError

HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "captures")
os.makedirs(OUT, exist_ok=True)


def exec_run(args, name):
    env = dict(os.environ, CODEX_HOME=HOME)
    p = subprocess.Popen(["codex", *args], stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, env=env, text=True, cwd=WORK)
    frames = []
    with open(os.path.join(OUT, name + ".jsonl"), "w") as fh:
        for line in p.stdout:
            fh.write(line)
            try:
                frames.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    p.wait()
    return frames, p.stderr.read()


def first_thread_id(frames):
    for f in frames:
        if f.get("type") == "thread.started":
            return f.get("thread_id")
    return None


print("########## exec: original ##########")
f1, e1 = exec_run(["exec", "--json", "--skip-git-repo-check", "-s", "read-only",
                   "-C", WORK, "reply with the word one"], "s12_exec_orig")
origin = first_thread_id(f1)
print("origin thread_id:", origin)

print("\n########## exec resume <origin> ##########")
f2, e2 = exec_run(["exec", "--json", "--skip-git-repo-check", "-s", "read-only",
                   "-C", WORK, "resume", origin, "reply with the word two"],
                  "s12_exec_resume")
resumed = first_thread_id(f2)
print("resume reported thread_id:", resumed)
print("SAME as origin?          :", resumed == origin)
if e2.strip():
    print("stderr:", e2[:300])

# Which rollout files exist for these ids, and what do their metas say?
print("\n########## rollouts on disk ##########")
for wanted in filter(None, {origin, resumed}):
    hits = glob.glob(os.path.join(HOME, "sessions", "**",
                                  f"rollout-*{wanted}.jsonl"), recursive=True)
    print(f"\nid {wanted}: {len(hits)} rollout file(s)")
    for h in hits:
        meta = json.loads(open(h, errors="replace").readline())
        pay = {k: v for k, v in (meta.get("payload") or {}).items()
               if k != "base_instructions"}
        print("  ", os.path.basename(h))
        print("   meta:", json.dumps(pay)[:500])
        # count user/assistant turns to see whether history was replayed
        n = sum(1 for ln in open(h, errors="replace") if '"response_item"' in ln)
        print("   response_item lines:", n)

print("\n\n########## app-server: thread/resume id stability ##########")
SOCK_DIR = "/private/tmp/codex-probe-sock12"
SOCK = SOCK_DIR + "/as.sock"
shutil.rmtree(SOCK_DIR, ignore_errors=True)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s12-err.log", "w"),
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
            obj = json.loads(ws.recv())
            with lock:
                frames.append(obj)
    except Exception:
        pass


threading.Thread(target=reader, daemon=True).start()
nid = [0]


def call(method, params, timeout=120):
    nid[0] += 1
    rid = nid[0]
    ws.send_text(json.dumps({"jsonrpc": "2.0", "id": rid,
                             "method": method, "params": params}))
    end = time.time() + timeout
    while time.time() < end:
        with lock:
            for f in frames:
                if f.get("id") == rid and ("result" in f or "error" in f):
                    return f
        time.sleep(0.02)
    raise TimeoutError(method)


def notifs(m):
    with lock:
        return [f for f in frames if f.get("method") == m and "id" not in f]


call("initialize", {"clientInfo": {"name": "probe", "version": "0.0.1"},
                    "capabilities": {"experimentalApi": True}})
ws.send_text(json.dumps({"jsonrpc": "2.0", "method": "initialized", "params": {}}))

r = call("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                          "sandbox": "read-only"})["result"]
as_tid = r["thread"]["id"]
print("app-server thread/start id:", as_tid)
before = len(notifs("turn/completed"))
call("turn/start", {"threadId": as_tid,
                    "input": [{"type": "text", "text": "reply with the word three"}]})
t0 = time.time()
while time.time() - t0 < 180 and len(notifs("turn/completed")) == before:
    time.sleep(0.1)

rr = call("thread/resume", {"threadId": as_tid})
if "error" in rr:
    print("thread/resume error:", json.dumps(rr["error"])[:300])
else:
    th = rr["result"]["thread"]
    print("thread/resume reported id:", th["id"])
    print("SAME as start?           :", th["id"] == as_tid)
    print("forkedFromId:", th.get("forkedFromId"),
          "| parentThreadId:", th.get("parentThreadId"))

ws.close()
srv.terminate()
try:
    srv.wait(timeout=5)
except Exception:
    srv.kill()
shutil.rmtree(SOCK_DIR, ignore_errors=True)
