#!/usr/bin/env python3
"""Census `session_meta` key shapes across the whole store, by cli_version.

Why: the first line of a rollout is where identity and lineage live, and
`docs/agents.md` records a *flat* `parent_thread_id` + `thread_source:
subagent`, while the newest rollouts nest lineage under
`source.subagent.thread_spawn`. If both spellings exist, an adapter that reads
only one silently loses lineage for part of the store -- exactly the
vendor-key defect class, and here it would be invisible because the field
simply reads absent.

This prints which spelling appears under which cli_version, so the adapter can
be written to the union rather than to whichever file happened to be sampled.
"""

import glob
import json
import os
from collections import Counter, defaultdict

SESS = os.path.expanduser("~/.codex/sessions")


def flat_keys(obj, prefix=""):
    """Enumerate leaf-ish key paths, stopping at 2 levels below `source`."""
    out = []
    if isinstance(obj, dict):
        for k, v in obj.items():
            p = f"{prefix}.{k}" if prefix else k
            out.append(p)
            if isinstance(v, dict) and p.count(".") < 3:
                out.extend(flat_keys(v, p))
    return out


def main():
    files = sorted(glob.glob(os.path.join(SESS, "**", "rollout-*.jsonl"), recursive=True))
    by_version = defaultdict(Counter)
    version_count = Counter()
    lineage_spelling = Counter()
    source_variants = Counter()
    first_line_types = Counter()

    for p in files:
        try:
            head = open(p, errors="replace").readline().strip()
            obj = json.loads(head)
        except Exception:
            continue
        first_line_types[obj.get("type")] += 1
        pay = obj.get("payload") or {}
        if not isinstance(pay, dict):
            continue
        ver = pay.get("cli_version", "?")
        version_count[ver] += 1
        for k in flat_keys(pay):
            if k.startswith("base_instructions"):
                continue
            by_version[ver][k] += 1

        # Which lineage spelling does this file use?
        # NOTE: `source` is polymorphic -- an object in newer rollouts, a bare
        # string in older ones. Measured, not assumed; a `.get` on it panics.
        def dig(d, *path):
            cur = d
            for k in path:
                if not isinstance(cur, dict):
                    return None
                cur = cur.get(k)
            return cur

        nested = dig(pay, "source", "subagent", "thread_spawn", "parent_thread_id")
        flat = pay.get("parent_thread_id")
        ts = pay.get("thread_source")
        if nested and flat:
            lineage_spelling["both nested+flat"] += 1
        elif nested:
            lineage_spelling["nested source.subagent.thread_spawn.parent_thread_id"] += 1
        elif flat:
            lineage_spelling["flat parent_thread_id"] += 1
        if ts is not None:
            lineage_spelling[f"thread_source={json.dumps(ts)[:40]}"] += 1
        src = pay.get("source")
        if src is not None:
            if isinstance(src, dict):
                source_variants["obj:" + "|".join(sorted(src.keys()))] += 1
            else:
                source_variants["str:" + json.dumps(src)[:60]] += 1

    print("first-line `type` values:", dict(first_line_types))
    print("\ncli_version distribution (top 15):")
    for v, n in version_count.most_common(15):
        print(f"  {v}: {n}")

    print("\nlineage spellings:")
    for k, n in lineage_spelling.most_common():
        print(f"  {k}: {n}")

    print("\n`source` variants (by top-level keys):")
    for k, n in source_variants.most_common(12):
        print(f"  {k}: {n}")

    print("\nlineage-bearing keys per cli_version:")
    interesting = ["parent_thread_id", "thread_source", "forked_from_id",
                   "source.subagent", "source.subagent.thread_spawn",
                   "source.subagent.thread_spawn.parent_thread_id",
                   "source.fork", "agent_role", "agent_nickname",
                   "session_id", "id", "multi_agent_version", "agent_path",
                   "subagent_history_start_ordinal", "history_mode",
                   "context_window", "originator", "cwd", "git"]
    versions = [v for v, _ in version_count.most_common()]
    for v in versions:
        row = by_version[v]
        present = [k for k in interesting if row.get(k)]
        print(f"  {v} (n={version_count[v]}): {', '.join(present)}")


if __name__ == "__main__":
    main()
