#!/usr/bin/env python3
"""Scenario 07: app-server over a unix socket -- the real second-party channel.

`remote-control` is gated twice (ChatGPT auth, managed standalone install), so
it cannot be what a Sylph envagent uses. `app-server --listen unix://PATH` has
neither gate, and it is the door that matters: two independent clients on one
daemon holding one thread is exactly `docs/architecture.md`'s "observation is
multi-party; control is single-owner".

Three transport facts measured here, none of which the stdio mode suggests:

  - The unix listener speaks **WebSocket**, not NDJSON. A bare NDJSON frame
    gets the connection closed with no data, and the server logs `failed to
    upgrade control socket websocket connection: ... invalid token`.
  - The upgrade is accepted on ANY path (/, /control, /rpc, /ws, /readyz,
    /healthz all answer the same `101`), so the path carries no meaning.
  - Its HTTP parser rejects a mainstream client's handshake: the `websockets`
    package fails with "did not receive a valid HTTP response" because of the
    extra headers it sends. See miniws.py.

Also: the listen path's PARENT must be a real directory. On macOS /tmp is a
symlink to /private/tmp and codex refuses with "socket directory path exists
and is not a directory: /tmp". codex creates the parent 0700, socket 0600.

Settles: does a socket client get the full method surface? Do two clients both
see one thread's events? Can the second client drive a turn the first started?
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from miniws import UnixWebSocket, WSError

SOCK_DIR = "/private/tmp/codex-probe-sock"
SOCK = SOCK_DIR + "/as.sock"
HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
WORK = "/tmp/codex-probe-work"
CAP = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "captures", "s07_unix_socket.jsonl")
os.makedirs(os.path.dirname(CAP), exist_ok=True)
os.makedirs(WORK, exist_ok=True)
cap = open(CAP, "w")
caplock = threading.Lock()


def log(who, direction, obj):
    with caplock:
        cap.write(json.dumps({"who": who, "dir": direction, "t": time.time(),
                              "frame": obj}) + "\n")
        cap.flush()


class Client:
    def __init__(self, name):
        self.name = name
        self.ws = UnixWebSocket(SOCK)
        self.frames = []
        self.lock = threading.Lock()
        self.next_id = 1
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            while True:
                raw = self.ws.recv()
                try:
                    obj = json.loads(raw)
                except json.JSONDecodeError:
                    log(self.name, "in-raw", raw[:400])
                    continue
                log(self.name, "in", obj)
                with self.lock:
                    self.frames.append(obj)
        except WSError as e:
            log(self.name, "closed", str(e))

    def send(self, method, params=None, notify=False):
        fr = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            fr["params"] = params
        rid = None
        if not notify:
            rid = self.next_id
            self.next_id += 1
            fr["id"] = rid
        log(self.name, "out", fr)
        self.ws.send_text(json.dumps(fr))
        return rid

    def wait(self, rid, timeout=90):
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                for fr in self.frames:
                    if fr.get("id") == rid and ("result" in fr or "error" in fr):
                        return fr
            time.sleep(0.02)
        raise TimeoutError(f"{self.name}: no reply to {rid}")

    def notifs(self, method=None):
        with self.lock:
            return [f for f in self.frames if "method" in f and "id" not in f
                    and (method is None or f["method"] == method)]

    def initialize(self):
        rid = self.send("initialize", {
            "clientInfo": {"name": f"sylph-probe-{self.name}", "version": "0.0.1"},
            "capabilities": {"experimentalApi": True}})
        r = self.wait(rid, timeout=30)
        self.send("initialized", {}, notify=True)
        return r


shutil.rmtree(SOCK_DIR, ignore_errors=True)
env = dict(os.environ, CODEX_HOME=HOME)
srv = subprocess.Popen(["codex", "app-server", "--listen", f"unix://{SOCK}"],
                       stdout=subprocess.DEVNULL,
                       stderr=open("/tmp/codex-s07-err.log", "w"), env=env)
for _ in range(120):
    if os.path.exists(SOCK):
        break
    time.sleep(0.1)
print("socket present:", os.path.exists(SOCK))
print("socket mode:", oct(os.stat(SOCK).st_mode & 0o777),
      "| dir mode:", oct(os.stat(SOCK_DIR).st_mode & 0o777))

a, b = Client("A"), Client("B")
print("\nA initialize:", json.dumps(a.initialize())[:200])
print("B initialize:", json.dumps(b.initialize())[:200])

rid = a.send("thread/start", {"cwd": WORK, "approvalPolicy": "never",
                             "sandbox": "workspace-write"})
res = a.wait(rid, timeout=60)["result"]
tid = res["thread"]["id"]
print("\nA started thread:", tid)
time.sleep(1.5)
print("B saw thread/started for:", [f["params"]["thread"]["id"]
                                    for f in b.notifs("thread/started")])

rid = b.send("thread/loaded/list", {})
try:
    print("B thread/loaded/list:", json.dumps(b.wait(rid, timeout=30))[:400])
except Exception as e:
    print("B loaded/list:", e)

t0 = time.time()
rid = a.send("turn/start", {"threadId": tid, "input": [
    {"type": "text", "text": "Run `sleep 20` then reply with the word first."}]})
print("\nA turn/start ->", json.dumps(a.wait(rid, timeout=60))[:160])

turn_id = None
end = time.time() + 60
while time.time() < end and turn_id is None:
    for f in a.notifs("turn/started"):
        turn_id = f["params"]["turn"]["id"]
    time.sleep(0.05)
print("turn:", turn_id)

time.sleep(7)
print(f"\n[t={time.time()-t0:.1f}] B observed:",
      dict(Counter(f.get("method") for f in b.notifs())))

rid = b.send("turn/interrupt", {"threadId": tid, "turnId": turn_id})
try:
    print(f"\n[t={time.time()-t0:.1f}] B turn/interrupt ->",
          json.dumps(b.wait(rid, timeout=30))[:300])
except Exception as e:
    print("B interrupt:", e)

time.sleep(4)
print("A saw turn/completed statuses:",
      [f["params"]["turn"]["status"] for f in a.notifs("turn/completed")])
print("B saw turn/completed statuses:",
      [f["params"]["turn"]["status"] for f in b.notifs("turn/completed")])

rid = b.send("turn/start", {"threadId": tid, "input": [
    {"type": "text", "text": "reply with the word second"}]})
try:
    print("\nB turn/start on A's thread ->", json.dumps(b.wait(rid, timeout=90))[:250])
except Exception as e:
    print("B turn/start:", e)

time.sleep(6)
print("\nA's tally:", dict(Counter(f.get("method") for f in a.notifs())))
print("B's tally:", dict(Counter(f.get("method") for f in b.notifs())))

print("\nDid A observe the turn B started? A agentMessage items:")
for f in a.notifs("item/completed"):
    it = f["params"]["item"]
    if it.get("type") == "agentMessage":
        print("   A saw:", json.dumps(it.get("text"))[:100])
print("B agentMessage items:")
for f in b.notifs("item/completed"):
    it = f["params"]["item"]
    if it.get("type") == "agentMessage":
        print("   B saw:", json.dumps(it.get("text"))[:100])

a.ws.close()
b.ws.close()
srv.terminate()
try:
    srv.wait(timeout=5)
except Exception:
    srv.kill()
cap.close()
shutil.rmtree(SOCK_DIR, ignore_errors=True)
print("\ncapture:", CAP)
