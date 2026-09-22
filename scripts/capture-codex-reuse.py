#!/usr/bin/env python3
"""Retain a sanitized native root/child excerpt proving repeated child execution."""
import argparse
import copy
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("root", type=Path)
parser.add_argument("child", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def load(path):
    raw = path.read_bytes()
    lines = raw.splitlines(keepends=True)
    return raw, lines, [json.loads(line) for line in lines]


sources = {name: load(path) for name, path in [("root", args.root), ("child", args.child)]}
root_header = sources["root"][2][0]["payload"]
child_header = sources["child"][2][0]["payload"]
root_id, child_id = root_header["id"], child_header["id"]
child_path = child_header["agent_path"]
assert root_header["session_id"] == child_header["session_id"] == root_id
assert child_header["parent_thread_id"] == root_id
selected = {"root": {0}, "child": {0}}
calls = set()
for index, row in enumerate(sources["root"][2]):
    value = row.get("payload", {})
    if value.get("type") != "function_call" or value.get("namespace") != "collaboration":
        continue
    arguments = json.loads(value["arguments"])
    if arguments.get("target") == child_path or (
        value.get("name") == "spawn_agent" and arguments.get("task_name") == child_path.rsplit("/", 1)[-1]
    ):
        selected["root"].add(index)
        calls.add(value["call_id"])

for name, (_, _, rows) in sources.items():
    for index, row in enumerate(rows):
        value = row.get("payload", {})
        kind = value.get("type")
        item = value.get("item", {})
        local = name == "root" or row.get("ordinal", 0) >= child_header["subagent_history_start_ordinal"]
        if not local:
            continue
        if row["type"] == "event_msg" and kind in ["task_started", "task_complete", "turn_aborted"]:
            selected[name].add(index)
        if kind == "agent_message" and child_path in [value.get("author"), value.get("recipient")]:
            selected[name].add(index)
            if index and rows[index - 1]["type"] == "inter_agent_communication_metadata":
                selected[name].add(index - 1)
        if name == "root" and (
            kind == "function_call_output" and value.get("call_id") in calls
            or item.get("type") == "SubAgentActivity" and item.get("agent_thread_id") == child_id
        ):
            selected[name].add(index)
        if name == "child" and (
            kind == "message" and value.get("phase") == "final_answer"
            or item.get("type") == "AgentMessage" and item.get("phase") == "final_answer"
        ):
            selected[name].add(index)


def marker(text):
    return "SYLPH_REUSE_TEXT_" + digest(text.encode())[:16]


def sanitize(value):
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    if not isinstance(value, dict):
        return value
    result = {}
    for key, item in value.items():
        if key in ["text", "last_agent_message"] and isinstance(item, str):
            result[key] = marker(item)
        elif key == "arguments" and isinstance(item, str):
            arguments = json.loads(item)
            if isinstance(arguments.get("message"), str):
                arguments["message"] = marker(arguments["message"])
            result[key] = json.dumps(arguments, ensure_ascii=False, separators=(",", ":"))
        else:
            result[key] = sanitize(item)
    return result


args.output.mkdir(parents=True, exist_ok=True)
manifest = {
    "vendor": "codex", "version": root_header["cli_version"],
    "root": root_id, "child": child_id, "child_path": child_path,
    "method": "scripts/capture-codex-reuse.py ROOT CHILD OUTPUT; read-only extraction of native records, never execution of historical tool text",
    "limits": "Partial native sources, not a live/native pair or a complete history. Missing records, selected heads, request inclusion and unobserved joins remain unknown.",
    "sanitization": "Headers are structural extracts omitting cwd, git and base_instructions. Other selected records preserve all fields; prose text, last_agent_message and invocation message arguments become deterministic markers preserving equality. JSON is compactly reserialized. IDs, clocks, ordering and other values remain unchanged.",
    "sources": {},
}
for name, (raw, lines, rows) in sources.items():
    offsets = []
    offset = 0
    for line in lines:
        offsets.append(offset)
        offset += len(line)
    output = bytearray()
    records = []
    for index in sorted(selected[name]):
        value = copy.deepcopy(rows[index])
        if index == 0:
            for field in ["cwd", "git", "base_instructions"]:
                value["payload"].pop(field, None)
        value = sanitize(value)
        retained = (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        output.extend(retained)
        records.append({
            "line": index + 1, "byte_start": offsets[index], "byte_end": offsets[index] + len(lines[index]),
            "vendor_ordinal": rows[index].get("ordinal"), "source_sha256": digest(lines[index]),
            "retained_sha256": digest(retained),
        })
    (args.output / (name + ".jsonl")).write_bytes(output)
    path = args.root if name == "root" else args.child
    manifest["sources"][name] = {"locator": str(path), "source_sha256": digest(raw), "retained_sha256": digest(output), "records": records}
(args.output / "capture.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({name: len(selected[name]) for name in selected}))
