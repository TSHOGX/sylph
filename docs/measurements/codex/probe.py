#!/usr/bin/env python3
"""Drive `codex app-server` over stdio and capture every frame verbatim.

Why a harness rather than reading the schema: AGENTS.md's most-repeated defect
class is reading the wrong key off a vendor payload, and the schema is a
declaration. A declared method is not a promise anything answers it, and a
declared field is not a promise it is populated. So every claim in the report
is backed by a captured frame from this script.

Usage:
    CODEX_HOME=/tmp/codex-probe-home python3 probe.py <scenario> [args]

Every frame is appended to a per-scenario .jsonl with a direction marker,
so the capture is replayable and diffable.
"""

import json
import os
import subprocess
import sys
import threading
import time

HOME = os.environ.get("CODEX_HOME", "/tmp/codex-probe-home")
OUT_DIR = os.path.dirname(os.path.abspath(__file__))


class AppServer:
    def __init__(self, capture_name, extra_args=()):
        self.path = os.path.join(OUT_DIR, "captures", capture_name + ".jsonl")
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        self.cap = open(self.path, "w")
        env = dict(os.environ, CODEX_HOME=HOME, RUST_LOG="info")
        self.proc = subprocess.Popen(
            ["codex", "app-server", *extra_args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            bufsize=1,
        )
        self.lock = threading.Lock()
        self.frames = []           # every server frame, in arrival order
        self.next_id = 1
        self.stderr_lines = []
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _log(self, direction, obj):
        with self.lock:
            self.cap.write(json.dumps({"dir": direction, "t": time.time(), "frame": obj}) + "\n")
            self.cap.flush()

    def _read_stdout(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                self._log("in-raw", line)
                continue
            self._log("in", obj)
            with self.lock:
                self.frames.append(obj)

    def _read_stderr(self):
        for line in self.proc.stderr:
            self.stderr_lines.append(line.rstrip())
            self._log("stderr", line.rstrip())

    def send(self, method, params=None, is_notification=False):
        frame = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            frame["params"] = params
        rid = None
        if not is_notification:
            rid = self.next_id
            self.next_id += 1
            frame["id"] = rid
        self._log("out", frame)
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()
        return rid

    def respond(self, rid, result):
        frame = {"jsonrpc": "2.0", "id": rid, "result": result}
        self._log("out", frame)
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def wait_result(self, rid, timeout=120):
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                for f in self.frames:
                    if f.get("id") == rid and ("result" in f or "error" in f):
                        return f
            time.sleep(0.02)
        raise TimeoutError(f"no reply to id={rid} within {timeout}s")

    def wait_notification(self, method, timeout=120, after=0):
        """Wait for a notification by method name; returns (index, frame)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                for i, f in enumerate(self.frames):
                    if i >= after and f.get("method") == method:
                        return i, f
            time.sleep(0.02)
        raise TimeoutError(f"no {method} within {timeout}s")

    def notifications(self, method=None):
        with self.lock:
            return [f for f in self.frames
                    if "method" in f and (method is None or f["method"] == method)]

    def initialize(self, experimental=True):
        # `capabilities.experimentalApi` gates a whole tier of methods: without it
        # `remoteControl/*` and `collaborationMode/list` answer
        # `-32600 ... requires experimentalApi capability`. Measured on 0.150.1.
        rid = self.send("initialize", {
            "clientInfo": {"name": "sylph-probe", "title": "Sylph Probe", "version": "0.0.1"},
            "capabilities": {"experimentalApi": experimental},
        })
        rep = self.wait_result(rid, timeout=30)
        self.send("initialized", {}, is_notification=True)
        return rep

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
        self.cap.close()


def jp(label, obj):
    print(f"\n----- {label} -----")
    print(json.dumps(obj, indent=1)[:4000])
