#!/usr/bin/env python3
"""Read selected native histories; emit content-free, digest-pinned structural evidence.

This is a measurement, not a vendor parser or an adapter conformance test. It never
executes historical content and deliberately retains no prompts, source, or output.
"""

import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path


ROOTS = (
    "01a067f8-de0e-77a0-ac73-e484ccc9959a",
    "01a083c2-1389-77c2-a16c-302aa2bbf884",
    "01a09de6-c5c3-7060-875a-44cbd4a91a60",
)
REVERT = "01a07203-01f9-7ba2-bb00-5996103c01d9"
HEADER_FIELDS = (
    "id", "session_id", "cli_version", "history_mode", "parent_thread_id",
    "forked_from_id", "forked_from_ordinal_exclusive", "history_base",
    "subagent_history_start_ordinal",
)
IDENTITY_FIELDS = (
    "id", "call_id", "turn_id", "thread_id", "agent_thread_id", "session_id",
    "root_turn_id", "response_id", "process_id", "client_id",
)
BOUNDARY_FIELDS = (
    "turn_id", "started_at", "completed_at", "duration_ms",
    "time_to_first_token_ms", "reason",
)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def select(value, fields):
    return {key: value[key] for key in fields if key in value}


def content_fingerprint(value):
    data = json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False).encode()
    return {"json_bytes": len(data), "canonical_json_sha256": digest(data)}


def descriptor(record, line, raw):
    payload = record["payload"]
    result = {"line": line, "record_sha256": digest(raw), "type": record["type"],
              "payload_fields": sorted(payload)}
    result.update(select(payload, IDENTITY_FIELDS))
    if "ordinal" in record:
        result["vendor_ordinal"] = record["ordinal"]
    if isinstance(payload.get("type"), str):
        result["payload_type"] = payload["type"]
    if payload.get("type") in ("function_call", "custom_tool_call"):
        result["tool_route"] = select(payload, ("name", "namespace"))
    item = payload.get("item")
    if isinstance(item, dict):
        result["item"] = {"type": item["type"], "fields": sorted(item),
                          **select(item, IDENTITY_FIELDS)}
        # These are closed vendor semantic fields, never free-form content.
        result["item"].update(select(item, ("status", "kind", "durationMs")))
    for key in ("content", "input", "arguments", "output", "replacement_history"):
        if key in payload:
            result[key] = content_fingerprint(payload[key])
    return result


def read_file(path, root, detailed=False):
    sha = hashlib.sha256()
    kinds, events, responses, items, calls, activities = (Counter() for _ in range(6))
    call_ids, output_ids, response_ids = Counter(), Counter(), Counter()
    samples, boundaries, compactions = {}, [], []
    header = None
    first_local = None
    previous = None
    communication_pairs = 0
    inherited = Counter()
    size = 0
    records = 0
    first_ordinal = None
    last_ordinal = None
    for line_number, raw in enumerate(path.open("rb"), 1):
        sha.update(raw)
        size += len(raw)
        record = json.loads(raw)
        payload = record["payload"]
        if not isinstance(payload, dict):
            raise ValueError(f"Non-object payload at {path}:{line_number}")
        records += 1
        kind, subtype = record["type"], payload.get("type")
        if line_number == 1:
            if kind != "session_meta":
                raise ValueError(f"Missing session metadata: {path}")
            header = select(payload, HEADER_FIELDS)
            first_ordinal = record.get("ordinal")
        cutoff = header.get("subagent_history_start_ordinal")
        # Revert headers start at history_base.end_ordinal_exclusive. Their
        # vendor ordinal is not the physical line number or byte position.
        ordinal = record.get("ordinal")
        last_ordinal = ordinal
        if cutoff is not None and ordinal is None:
            raise ValueError(f"Cannot measure inherited cutoff without ordinal: {path}:{line_number}")
        if cutoff is not None and ordinal < cutoff:
            inherited[kind] += 1
        if cutoff is not None and ordinal == cutoff:
            first_local = descriptor(record, line_number, raw)
        if not detailed:
            continue
        kinds[kind] += 1
        if kind == "event_msg":
            events[subtype] += 1
            if subtype in ("task_started", "task_complete", "turn_aborted"):
                boundary = {"line": line_number, "type": subtype,
                            **select(payload, BOUNDARY_FIELDS),
                            "has_error": payload.get("error") is not None}
                if "started_at" in payload and "completed_at" in payload:
                    boundary["wall_elapsed_ms_from_seconds"] = (
                        payload["completed_at"] - payload["started_at"]) * 1000
                boundaries.append(boundary)
            item = payload.get("item", {})
            if subtype == "item_completed":
                items[item["type"]] += 1
                if item["type"] == "SubAgentActivity":
                    activities[item["kind"]] += 1
                key = "item:" + item["type"]
                samples.setdefault(key, descriptor(record, line_number, raw))
        if kind == "response_item":
            responses[subtype] += 1
            if subtype in ("function_call", "custom_tool_call"):
                calls[payload["name"]] += 1
                call_ids[payload["call_id"]] += 1
            if subtype in ("function_call_output", "custom_tool_call_output"):
                output_ids[payload["call_id"]] += 1
            if subtype == "agent_message" and previous == "inter_agent_communication_metadata":
                communication_pairs += 1
            samples.setdefault("response:" + subtype, descriptor(record, line_number, raw))
        if kind == "token_usage_record":
            response_ids[payload["response_id"]] += 1
            if "usage" not in samples:
                samples["usage"] = descriptor(record, line_number, raw)
                samples["usage"].update(select(payload, (
                    "usage", "turn_token_usage", "thread_token_usage")))
        if kind == "compacted":
            compactions.append({"line": line_number, "payload_fields": sorted(payload),
                                "replacement_items": len(payload.get("replacement_history") or [])})
        previous = kind
    result = {"file": str(path.relative_to(root)), "bytes": size,
              "sha256": sha.hexdigest(), "records": records, "header": header,
              "first_vendor_ordinal": first_ordinal, "last_vendor_ordinal": last_ordinal}
    if cutoff is not None:
        result.update({"inherited_record_types_including_header": dict(inherited),
                       "first_local_record": first_local})
    if detailed:
        result.update({"record_types": dict(kinds), "event_types": dict(events),
                       "response_types": dict(responses), "item_types": dict(items),
                       "call_names": dict(calls), "activity_kinds": dict(activities),
                       "communication_metadata_pairs": communication_pairs,
                       "unique_response_usage_ids": len(response_ids),
                       "repeated_response_usage_ids": sum(n > 1 for n in response_ids.values()),
                       "calls_without_observed_output": sum((call_ids - output_ids).values()),
                       "boundaries": boundaries, "compactions": compactions,
                       "samples": samples})
    return result


def collect(root):
    files, parents = defaultdict(list), defaultdict(set)
    for path in sorted(root.rglob("rollout-*.jsonl")):
        with path.open("rb") as stream:
            head = json.loads(next(stream))
        if head["type"] != "session_meta":
            raise ValueError(f"Missing session metadata: {path}")
        metadata = head["payload"]
        thread = metadata["id"]
        files[thread].append(path)
        source = metadata.get("source", {})
        subagent = source.get("subagent", {}) if isinstance(source, dict) else {}
        spawn = subagent.get("thread_spawn", {}) if isinstance(subagent, dict) else {}
        parent = metadata.get("parent_thread_id") or spawn.get("parent_thread_id")
        if parent:
            parents[thread].add(parent)
    result = {"research_date": "2026-09-15", "source_root": "~/.codex/sessions",
              "source_revision": "968835997714baaff199cfed5f89a2c65d8ca77d",
              "coverage": "Structural observations only; no semantic adapter replay.",
              "roots": [], "revert_lineage": []}
    for thread in ROOTS:
        if not files[thread]:
            raise FileNotFoundError(f"Missing research thread: {thread}")
        children = sorted(child for child, values in parents.items() if thread in values)
        result["roots"].append({"thread_id": thread, "direct_child_ids": children,
                                "files": [read_file(path, root, True) for path in files[thread]],
                                "child_files": [read_file(path, root) for child in children
                                                for path in files[child]]})
    if not files[REVERT]:
        raise FileNotFoundError(f"Missing research thread: {REVERT}")
    result["revert_lineage"] = [read_file(path, root) for path in files[REVERT]]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.home() / ".codex" / "sessions")
    parser.add_argument("--check", type=Path, help="Compare against an existing inventory; write nothing")
    args = parser.parse_args()
    actual = collect(args.root)
    if args.check:
        expected = json.loads(args.check.read_text())
        if actual != expected:
            raise SystemExit("Codex structural evidence differs from the pinned inventory")
        print("Codex structural evidence matches the pinned inventory")
    else:
        print(json.dumps(actual, ensure_ascii=False, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
