#!/usr/bin/env python3
"""Audit retained Claude/Pi structural evidence without reading private stores by default."""

import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import sys


class AuditFailure(Exception):
    """An evidence digest or independently stated semantic promise did not hold."""


def require(condition, message):
    if not condition:
        raise AuditFailure(message)


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def check_bytes(path, item):
    raw = path.read_bytes()
    require(len(raw) == item["byte_length"], f"{path.name}: byte length differs")
    require(hashlib.sha256(raw).hexdigest() == item["sha256"], f"{path.name}: SHA-256 differs")
    if "line_count" in item:
        require(len(raw.splitlines()) == item["line_count"], f"{path.name}: line count differs")


def audit_claude(records, sources):
    source_id = next(
        item["source_id"] for item in sources
        if item["locator"].endswith("/ordinary-turn-session-start.jsonl")
    )
    groups = defaultdict(list)
    for record in records:
        message = record.get("message", {})
        if record["source_id"] == source_id and "id" in message:
            groups[message["id"]].append(record)
    require(len(groups) == 1, "Claude: the observed model response grouping changed")
    parts = next(iter(groups.values()))
    require(len(parts) == 4, "Claude: one model response must preserve four record occurrences")
    require(len({r["uuid"] for r in parts}) == 4,
            "Claude: shared model message ID must not collapse distinct record UUIDs")
    addresses = {(r["uuid"], p["source_block_index"])
                 for r in parts for p in r["message"]["content_structure"]}
    require(len(addresses) == 4, "Claude: each native content part needs its own scoped address")
    kinds = Counter(p["type"] for r in parts for p in r["message"]["content_structure"])
    require(kinds == {"thinking": 1, "text": 1, "tool_use": 2},
            "Claude: thinking, prose, and both tool calls must survive")

    calls = {}
    returned_agents = {}
    for record in records:
        content = record.get("message", {}).get("content_structure", [])
        if not isinstance(content, list):
            continue
        for part in content:
            if part.get("type") == "tool_use":
                calls[part["id"]] = record.get("agentId")
            if part.get("type") == "tool_result":
                agent = record.get("toolUseResult", {}).get("agentId")
                if agent is not None:
                    returned_agents[part["tool_use_id"]] = agent
    children = [r for r in records if r["type"] == "agent_metadata_sidecar"]
    child_ids = {r["agentId_from_filename"] for r in children}
    require(len(child_ids) == len(children) == 6, "Claude: six explicitly identified children must survive")
    nested = [r for r in children if "parentAgentId" in r]
    require(len(nested) == 3, "Claude: three children have explicit nested parents")
    for child in children:
        call = child["toolUseId"]
        require(call in calls, "Claude: child metadata must resolve its actual invocation")
        require(calls[call] == child.get("parentAgentId"),
                "Claude: spawn parent must agree with the invoking agent, not directory depth")
        if "parentAgentId" in child:
            require(child["parentAgentId"] in child_ids, "Claude: explicit parent reference must resolve")
        else:
            require(returned_agents.get(call) == child["agentId_from_filename"],
                    "Claude: launch result and sidecar must name the same actual child")


def audit_pi(records):
    require(records[0]["type"] == "session", "Pi: source begins with its session header")
    entries = {r["id"]: r for r in records[1:]}
    require(len(entries) == len(records) - 1, "Pi: distinct native entries must not collapse")
    parents = defaultdict(list)
    for entry in entries.values():
        parent = entry.get("parentId", entry.get("parent_id"))
        require(parent is None or parent in entries, "Pi: every captured non-root predecessor resolves")
        parents[parent].append(entry)
    siblings = [items for parent, items in parents.items() if parent is not None and len(items) > 1]
    require(sorted(map(len, siblings)) == [2, 2], "Pi: the observed history has two distinct branch points")
    require(all(e.get("message", {}).get("role") == "user" for pair in siblings for e in pair),
            "Pi: each observed branch point preserves two user-message alternatives")

    by_line = {r["source_line"]: r for r in records}
    for left, right, predecessor in [(12, 14, 11), (42, 44, 41)]:
        require(by_line[left]["parentId"] == by_line[right]["parentId"] == by_line[predecessor]["id"],
                f"Pi: source lines {left} and {right} extend their shared predecessor")
    # Inspect the branch ending at the last recorded entry, without claiming it is
    # a running process's current selected head. Physical adjacency is not ancestry.
    path = set()
    cursor = records[-1]["id"]
    while cursor is not None:
        require(cursor not in path, "Pi: native predecessor graph must be acyclic")
        path.add(cursor)
        entry = entries[cursor]
        cursor = entry.get("parentId", entry.get("parent_id"))
    for line in (14, 44, 11, 41):
        require(by_line[line]["id"] in path, "Pi: chosen branch keeps its own inputs and common ancestors")
    for line in (12, 13, 42, 43):
        require(by_line[line]["id"] not in path, "Pi: a branch must not consume its abandoned sibling path")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path, default=Path(__file__).resolve().parent / "fixtures")
    parser.add_argument("--sources", action="store_true", help="also verify available local original source bytes")
    args = parser.parse_args()
    inventory = read_json(args.fixtures / "vendor-structure-inventory.json")
    for artifact in inventory["artifacts"]:
        relative = Path(artifact["path"])
        require(not relative.is_absolute() and ".." not in relative.parts, "Artifact path must remain inside fixtures")
        check_bytes(args.fixtures / relative, artifact)
    claude = read_json(args.fixtures / "claude-structure.json")["records"]
    pi = read_json(args.fixtures / "pi-branch-structure.json")["records"]
    known_sources = {s["source_id"]: s for s in inventory["sources"]}
    for record in claude + pi:
        require(record["source_id"] in known_sources, "Every extract record names an inventoried source")
        require(1 <= record["source_line"] <= known_sources[record["source_id"]]["line_count"],
                "Every extract line falls within its captured original source")
    audit_claude(claude, inventory["sources"])
    audit_pi(pi)
    print("PASS: 2 artifact digests; 4 distinct Claude parts sharing one model message; "
          "6 explicit children with 3 nested parents; 2 Pi branch points and isolated sibling paths.")
    if args.sources:
        repository = Path(__file__).resolve().parents[3]
        checked = missing = 0
        for source in inventory["sources"]:
            path = Path(source["locator"]).expanduser()
            if not path.is_absolute():
                path = repository / path
            if not path.exists():
                missing += 1
                continue
            check_bytes(path, source)
            checked += 1
        print(f"Source verification: {checked} available originals matched; {missing} unavailable originals skipped.")


if __name__ == "__main__":
    try:
        main()
    except (AuditFailure, OSError, ValueError, KeyError, StopIteration) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
