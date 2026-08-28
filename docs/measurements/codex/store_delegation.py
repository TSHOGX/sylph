#!/usr/bin/env python3
"""Measure codex delegation as it appears in the native store.

Two claims to settle, both from `docs/agents.md` § codex:
  - "a subagent may record only a parent id with no id-suffix marker"
  - the concrete shape: a `function_call` named `spawn_agent` whose
    `function_call_output` carries an `agent_id`.

Prints the real records rather than a summary, because a field name inferred
from a parser is still an inference.
"""

import glob
import json
import os
import sys
from collections import Counter

SESS = os.path.expanduser("~/.codex/sessions")


def rollouts():
    return sorted(glob.glob(os.path.join(SESS, "**", "rollout-*.jsonl"), recursive=True))


def lines(path):
    with open(path, errors="replace") as fh:
        for ln in fh:
            ln = ln.strip()
            if not ln:
                continue
            try:
                yield json.loads(ln)
            except json.JSONDecodeError:
                continue


def main():
    files = rollouts()
    print(f"rollout files: {len(files)}")

    spawn_files = []
    subagent_meta = []
    parent_keys = Counter()
    for p in files:
        try:
            head = open(p, errors="replace").readline()
        except OSError:
            continue
        raw = head
        if '"subagent"' in raw or "parent_thread_id" in raw or "parent_turn_id" in raw:
            try:
                obj = json.loads(raw)
            except json.JSONDecodeError:
                obj = None
            if obj:
                subagent_meta.append((p, obj))
        with open(p, errors="replace") as fh:
            body = fh.read()
        if "spawn_agent" in body:
            spawn_files.append(p)

    print(f"files whose first line mentions subagent/parent: {len(subagent_meta)}")
    print(f"files containing spawn_agent: {len(spawn_files)}")

    if subagent_meta:
        print("\n===== first line of a subagent rollout (verbatim) =====")
        p, obj = subagent_meta[0]
        print("file:", os.path.basename(p))
        print(json.dumps(obj, indent=1))
        for _, o in subagent_meta:
            payload = o.get("payload", o)
            for k in payload:
                parent_keys[k] += 1
        print("\nkeys present on subagent-rollout meta payloads:")
        for k, n in parent_keys.most_common():
            print(f"  {k}: {n}")

    # Now the live-ish delegation shape: spawn_agent call and its output.
    if spawn_files:
        print("\n===== spawn_agent function_call / output (verbatim) =====")
        shown = 0
        for p in spawn_files:
            for obj in lines(p):
                payload = obj.get("payload", {})
                if not isinstance(payload, dict):
                    continue
                name = payload.get("name")
                t = payload.get("type")
                if name == "spawn_agent" or (t == "function_call" and name and "agent" in str(name)):
                    print(f"\n--- {os.path.basename(p)} :: {t} ---")
                    print(json.dumps(obj, indent=1)[:2500])
                    shown += 1
                elif t == "function_call_output" and "agent_id" in json.dumps(payload):
                    print(f"\n--- {os.path.basename(p)} :: function_call_output ---")
                    print(json.dumps(obj, indent=1)[:2500])
                    shown += 1
                if shown >= 6:
                    return
            if shown >= 6:
                return


if __name__ == "__main__":
    main()
