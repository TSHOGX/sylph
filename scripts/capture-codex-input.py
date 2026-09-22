#!/usr/bin/env python3
"""Capture real Codex input correlation against a bounded loopback provider."""

import hashlib
import json
import os
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(tempfile.mkdtemp(prefix="sylph-codex-input-"))
print(root, flush=True)
codex_root = root / "config"
codex_root.mkdir()
work = root / "work"
work.mkdir()
requests = []


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        if (
            self.path != "/v1/responses"
            or self.headers.get("authorization") != "Bearer local-fixture"
        ):
            self.send_error(400)
            return
        length = int(self.headers.get("content-length", "0"))
        if length > 4 * 1024 * 1024:
            self.send_error(413)
            return
        raw = self.rfile.read(length)
        requests.append((self.path, raw))
        ordinal = len(requests)
        if ordinal > 4:
            self.send_error(429)
            return
        (root / f"provider-{ordinal}.bin").write_bytes(raw)
        events = [
            {"type": "response.created", "response": {"id": f"resp_fixture_{ordinal}"}},
            {
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "id": f"msg_fixture_{ordinal}",
                    "phase": "final_answer",
                    "content": [
                        {"type": "output_text", "text": f"SYLPH_CODEX_CLIENT_{ordinal}"}
                    ],
                },
            },
            {
                "type": "response.completed",
                "response": {
                    "id": f"resp_fixture_{ordinal}",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 1,
                        "total_tokens": 11,
                        "input_tokens_details": None,
                        "output_tokens_details": None,
                    },
                },
            },
        ]
        body = "".join(
            f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
(codex_root / "config.toml").write_text(f"""model = "gpt-5.5"
model_provider = "fixture"
cli_auth_credentials_store = "file"
approval_policy = "never"
sandbox_mode = "read-only"
[model_providers.fixture]
name = "Local fixture"
base_url = "http://127.0.0.1:{server.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
env_key = "SYLPH_FIXTURE_KEY"
supports_websockets = false
""")
env = {
    "PATH": os.environ["PATH"],
    "CODEX_HOME": str(codex_root),
    "RUST_LOG": "warn",
    "NO_COLOR": "1",
    "SYLPH_FIXTURE_KEY": "local-fixture",
}
frames = []
condition = threading.Condition()
err = (root / "stderr.log").open("wb")
incoming = (root / "stdout.jsonl").open("wb")
outgoing = (root / "stdin.jsonl").open("wb")
wire = (root / "wire.jsonl").open("w")
wire_lock = threading.Lock()


def retain_frame(direction, frame):
    with wire_lock:
        wire.write(
            json.dumps({"direction": direction, "frame": frame}, separators=(",", ":"))
            + "\n"
        )
        wire.flush()


proc = subprocess.Popen(
    ["codex", "app-server"],
    cwd=work,
    env=env,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=err,
)


def read():
    for line in proc.stdout:
        incoming.write(line)
        incoming.flush()
        value = json.loads(line)
        retain_frame("in", value)
        with condition:
            frames.append(value)
            condition.notify_all()


reader = threading.Thread(target=read, daemon=True)
reader.start()
request_id = 0


def send(method, params, notification=False):
    global request_id
    frame = {"jsonrpc": "2.0", "method": method, "params": params}
    if not notification:
        request_id += 1
        frame["id"] = request_id
    raw = (json.dumps(frame, separators=(",", ":")) + "\n").encode()
    outgoing.write(raw)
    outgoing.flush()
    retain_frame("out", frame)
    proc.stdin.write(raw)
    proc.stdin.flush()
    return frame.get("id")


def wait(predicate):
    deadline = time.monotonic() + 20
    with condition:
        while True:
            found = next((frame for frame in frames if predicate(frame)), None)
            if found is not None:
                return found
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("No expected app-server frame")
            condition.wait(min(remaining, 1))


def rpc(method, params):
    rid = send(method, params)
    reply = wait(
        lambda frame: frame.get("id") == rid and ("result" in frame or "error" in frame)
    )
    if "error" in reply:
        raise RuntimeError(reply["error"])
    return reply["result"]


try:
    rpc(
        "initialize",
        {
            "clientInfo": {
                "name": "sylph-client-probe",
                "title": "Sylph client probe",
                "version": "1",
            },
            "capabilities": {"experimentalApi": True},
        },
    )
    send("initialized", {}, True)
    result = rpc(
        "thread/start",
        {
            "cwd": str(work),
            "model": "gpt-5.5",
            "modelProvider": "fixture",
            "approvalPolicy": "never",
            "sandbox": "read-only",
        },
    )
    native = result["thread"]["id"]
    for index in [1, 2]:
        result = rpc(
            "turn/start",
            {
                "threadId": native,
                "clientUserMessageId": f"sylph:input:fixture-command-{index}",
                "input": [{"type": "text", "text": "SYLPH_IDENTICAL_SUBMITTED_INPUT"}],
            },
        )
        turn = result["turn"]["id"]
        wait(
            lambda frame: (
                frame.get("method") == "turn/completed"
                and frame["params"]["turn"]["id"] == turn
            )
        )
    print(
        json.dumps(
            {
                "thread": native,
                "requests": len(requests),
                "user_items": [
                    frame["params"]["item"]
                    for frame in frames
                    if frame.get("method") == "item/completed"
                    and frame.get("params", {}).get("item", {}).get("type")
                    == "userMessage"
                ],
            }
        ),
        flush=True,
    )
finally:
    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    reader.join(timeout=5)
    server.shutdown()
    server.server_close()
    incoming.close()
    outgoing.close()
    wire.close()
    err.close()

# Preserve complete frames and records; substitute only the isolated root path.
paths = list((codex_root / "sessions").rglob("*.jsonl"))
if len(paths) != 1:
    raise RuntimeError("Expected one retained native transcript")
version = subprocess.check_output(["codex", "--version"], env=env, text=True).strip()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def retained(raw):
    def replace(value):
        if isinstance(value, str):
            return value.replace(
                str(root.resolve()), "/workspace/codex-capture"
            ).replace(str(root), "/workspace/codex-capture")
        if isinstance(value, list):
            return [replace(item) for item in value]
        if isinstance(value, dict):
            return {key: replace(item) for key, item in value.items()}
        return value

    return b"".join(
        (
            json.dumps(
                replace(json.loads(line)), ensure_ascii=False, separators=(",", ":")
            )
            + "\n"
        ).encode()
        for line in raw.splitlines()
    )


sources = {
    name: (root / (name + ".jsonl")).read_bytes()
    for name in ["stdin", "stdout", "wire"]
}
sources["native"] = paths[0].read_bytes()
manifest = {
    "vendor": "codex",
    "version": version,
    "session_id": native,
    "method": "scripts/capture-codex-input.py; real app-server with isolated configuration and a loopback Responses provider. Only a checked local fixture API key is supplied. Two identical human-shaped input values have independent client correlation IDs. No remote model or requested tool execution.",
    "limits": "Provider response text and usage are synthetic; all app-server frames, vendor identifiers, counters and native persistence are observed. Client correlation does not establish inference inclusion or authorize dispatch retries.",
    "sanitization": "Substitute only the temporary root path and compactly reserialize complete JSON frames and native records. No fields or records are removed.",
    "source_sha256": {},
    "retained_sha256": {},
    "records": {},
}
for name, raw in sources.items():
    output = retained(raw)
    (root / (name + "-retained.jsonl")).write_bytes(output)
    manifest["source_sha256"][name] = digest(raw)
    manifest["retained_sha256"][name] = digest(output)
    manifest["records"][name] = len(raw.splitlines())
items = [
    frame["params"]["item"]
    for frame in frames
    if frame.get("method") == "item/completed"
    and frame.get("params", {}).get("item", {}).get("type") == "userMessage"
]
if (
    len(items) != 2
    or [item.get("clientId") for item in items]
    != ["sylph:input:fixture-command-1", "sylph:input:fixture-command-2"]
    or items[0]["id"] == items[1]["id"]
):
    raise RuntimeError(
        "Input correlation did not match distinct observed native message IDs"
    )
(root / "capture.json").write_text(json.dumps(manifest, indent=2) + "\n")
