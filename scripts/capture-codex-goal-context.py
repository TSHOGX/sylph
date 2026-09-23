#!/usr/bin/env python3
"""Capture typed goal context and an identical ordinary input with a loopback provider."""

import hashlib
import json
import os
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(tempfile.mkdtemp(prefix="sylph-codex-goal-context-"))
print(root, flush=True)
codex_root = root / "config"
codex_root.mkdir()
work = root / "work"
work.mkdir()
requests = []
provider_release = threading.Event()


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
        (root / f"provider-headers-{len(requests)}.json").write_text(json.dumps(dict(self.headers), indent=2) + "\n")
        ordinal = len(requests)
        if ordinal > 4:
            self.send_error(429)
            return
        (root / f"provider-{ordinal}.bin").write_bytes(raw)
        with condition:
            condition.notify_all()
        if ordinal == 2 and not provider_release.wait(timeout=20):
            self.send_error(503)
            return
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
                        {"type": "output_text", "text": f"SYLPH_GOAL_PROVIDER_{ordinal}"}
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
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass


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
    goal = rpc("thread/goal/set", {
        "threadId": native,
        "objective": "SYLPH_CONTROLLED_GOAL_CONTEXT",
        "tokenBudget": 100,
    })
    if (
        goal["goal"]["threadId"] != native
        or goal["goal"]["objective"] != "SYLPH_CONTROLLED_GOAL_CONTEXT"
        or goal["goal"]["status"] != "active"
        or goal["goal"]["tokenBudget"] != 100
    ):
        raise RuntimeError("Goal creation did not return the requested state")
    (root / "goal-set.json").write_text(json.dumps(goal, indent=2) + "\n")
    # Hold the second goal inference open so pausing and interrupting have an
    # explicit window; no timing race can turn the ordinary input into a steer.
    with condition:
        if not condition.wait_for(lambda: len(requests) >= 2, timeout=20):
            raise TimeoutError("No second goal provider request")
    second_metadata = json.loads(json.loads(requests[1][1])["client_metadata"]["x-codex-turn-metadata"])
    active_turn = second_metadata["turn_id"]
    wait(lambda frame: frame.get("method") == "turn/started" and frame["params"]["turn"]["id"] == active_turn)
    paused = rpc("thread/goal/set", {"threadId": native, "status": "paused"})
    if paused["goal"]["threadId"] != native or paused["goal"]["status"] != "paused":
        raise RuntimeError("Goal pause did not take effect")
    (root / "goal-paused.json").write_text(json.dumps(paused, indent=2) + "\n")
    rpc("turn/interrupt", {"threadId": native, "turnId": active_turn})
    interrupted = wait(lambda frame: frame.get("method") == "turn/completed" and frame["params"]["turn"]["id"] == active_turn)
    if interrupted["params"]["turn"]["status"] != "interrupted":
        raise RuntimeError("The held goal turn did not report interruption")
    provider_release.set()
    native_file = next((codex_root / "sessions").rglob("*.jsonl"))
    contexts = []
    for raw_line in native_file.read_bytes().splitlines():
        record = json.loads(raw_line)
        item = record.get("payload", {})
        if item.get("internal_chat_message_metadata_passthrough", {}).get("content_item_kinds") == ["goal.internal_context"]:
            contexts.append(item["content"][0]["text"])
    if not contexts:
        raise RuntimeError("The native store did not retain a typed goal context")
    literal = rpc("turn/start", {
        "threadId": native,
        "clientUserMessageId": "fixture:literal-goal-context",
        "input": [{"type": "text", "text": contexts[0]}],
    })["turn"]["id"]
    wait(lambda frame: frame.get("method") == "turn/completed" and frame["params"]["turn"]["id"] == literal)
    print(json.dumps({"thread": native, "requests": len(requests), "interrupted_goal_turn": active_turn, "literal_turn": literal}), flush=True)

finally:
    provider_release.set()
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

subprocess.run([
    "python3", str(Path(__file__).with_name("capture-codex-goal.py")),
    "--database", str(codex_root / "goals_1.sqlite"), native, str(root / "goal-snapshot"),
], check=True, stdout=subprocess.DEVNULL)

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
sources["provider"] = b"".join(raw + b"\n" for _, raw in requests)
(root / "provider.jsonl").write_bytes(sources["provider"])
sources["provider_headers"] = b"".join(
    (json.dumps(json.loads(path.read_text()), separators=(",", ":")) + "\n").encode()
    for path in sorted(root.glob("provider-headers-*.json"))
)
(root / "provider_headers.jsonl").write_bytes(sources["provider_headers"])
manifest = {
    "vendor": "codex",
    "version": version,
    "session_id": native,
    "method": "scripts/capture-codex-goal-context.py; real isolated app-server and bounded loopback Responses provider. Set an isolated goal, hold the second provider request, pause the goal and interrupt that observed turn before releasing it, then submit its exact context text as an ordinary new turn. Only the checked local fixture key is supplied; no remote model or requested tool execution.",
    "limits": "Provider output and usage are synthetic. Vendor IDs, goal state, wire frames, native persistence and provider-request annotations are observed. Provider requests are research witnesses, not an acquisition surface available to Sylph. Provider headers are a logical field map, not original HTTP framing. A context annotation does not identify a goal instance or prove a turn trigger; matching text cannot supply either.",
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

records = [json.loads(line) for line in sources["native"].splitlines()]
messages = [record["payload"] for record in records if record.get("type") == "response_item" and record.get("payload", {}).get("type") == "message"]
def kinds(item):
    return item.get("internal_chat_message_metadata_passthrough", {}).get("content_item_kinds")
def text(item):
    return "".join(part.get("text", "") for part in item.get("content", []))
goal_messages = [item for item in messages if kinds(item) == ["goal.internal_context"]]
literal_messages = [item for item in messages if kinds(item) == ["user.text"] and any(text(item) == text(goal) for goal in goal_messages)]
if len(goal_messages) < 2 or len(literal_messages) != 1:
    raise RuntimeError("Missing observed goal continuation or its exact ordinary-input counterexample")
provider_metadata = [json.loads(json.loads(raw).get("client_metadata", {}).get("x-codex-turn-metadata", "{}")) for _, raw in requests]
if sum(item.get("turn_trigger") == "goal" for item in provider_metadata) < 2:
    raise RuntimeError("Provider metadata did not certify two goal-triggered requests")
if not any(item.get("turn_id") == literal and item.get("turn_trigger") is None for item in provider_metadata):
    raise RuntimeError("The ordinary literal input did not start a non-goal provider request")
if any("turn_trigger" in record.get("payload", {}) for record in records):
    raise RuntimeError("Native trigger observability changed; review the capture contract")
manifest["observed"] = {
    "goal_context_message_ids": [item["id"] for item in goal_messages],
    "literal_message_id": literal_messages[0]["id"],
    "literal_turn_id": literal,
    "equal_goal_text_message_ids": [item["id"] for item in goal_messages if text(item) == text(literal_messages[0])],
    "goal_request_turn_ids": [item["turn_id"] for item in provider_metadata if item.get("turn_trigger") == "goal"],
    "native_turn_ids": [record["payload"]["turn_id"] for record in records if record.get("payload", {}).get("type") == "task_started"],
}
manifest["goal_snapshot"] = json.loads((root / "goal-snapshot" / "capture.json").read_text())
(root / "capture.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"root": str(root), "requests": len(requests), "frames": len(frames), "goal_contexts": len(goal_messages)}), flush=True)
