#!/usr/bin/env python3
"""Join browser measurements with envagent timings from the same fixture run.

Usage: python3 collect-file-timing.py REPORT_JSON PLAYWRIGHT_LOG JOURNAL_DB
The journal and log must belong to REPORT_JSON's run, before another npm E2E reset.
"""
import datetime
import json
import pathlib
import re
import sqlite3
import sys

report_path, log_path, journal_path = map(pathlib.Path, sys.argv[1:])
report = json.loads(report_path.read_text())
log = re.sub(r"\x1b\[[0-9;]*m", "", log_path.read_text())
pattern = re.compile(r"(\d{4}-\d\d-\d\dT\S+) DEBUG .*file_list_timing .*relative=(\S*) queue_us=(\d+) filesystem_us=(\d+)")
entries = {}
for stamp, relative, queue, filesystem in pattern.findall(log):
    entries.setdefault(relative, []).append({
        "envQueueMs": int(queue) / 1000,
        "envFilesystemMs": int(filesystem) / 1000,
        "envFinishedAtMs": datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() * 1000,
    })
if "backgroundExecution" in report:
    timing = report["backgroundExecution"]
    background = (timing["turnId"], timing["startedAtMs"], timing["completedAtMs"])
else:
    with sqlite3.connect(f"file:{journal_path}?mode=ro", uri=True) as db:
        background = db.execute(
            "SELECT id,started_at_ms,completed_at_ms FROM turns WHERE request_text=? ORDER BY created_at_ms DESC LIMIT 1",
            ("bulk render please",),
        ).fetchone()
    assert background, "No background execution found in this run's journal"
    report["backgroundExecution"] = dict(zip(("turnId", "startedAtMs", "completedAtMs"), background))
for sample in report["samples"]:
    matches = entries.get(sample["relative"], [])
    assert len(matches) == 1, f"Expected one measured enumeration for {sample['relative']}, found {len(matches)}"
    sample.update(matches[0])
    residual = sample["server"]["bridge"] - sample["envQueueMs"] - sample["envFilesystemMs"]
    assert residual >= -0.002, "Envagent duration exceeds its enclosing bridge interval"
    sample["bridgeOutsideEnvMs"] = residual
    sample["backgroundActiveAtEnumeration"] = (
        background[1] <= sample["envFinishedAtMs"] <= background[2]
        if background[1] is not None and background[2] is not None else None
    )
first_sample = report["samples"][0]["envFinishedAtMs"]
report["firstFile"].update([entry for entry in entries[""] if entry["envFinishedAtMs"] <= first_sample][-1])
report["measurementNotes"] = [
    "Deterministic fixture agents; no model calls and no native user records.",
    "Browser durations and envagent durations use their own monotonic clocks; enumeration overlap uses same-host wall clocks.",
    "bridgeOutsideEnvMs includes server scheduling, transport, and reply decoding, not just network time.",
    "Heap samples do not force garbage collection and are not a leak measurement.",
    "Background activity is recorded per sample; do not claim every expansion overlapped execution.",
]
report_path.write_text(json.dumps(report, indent=2) + "\n")
print(f"Joined {len(report['samples'])} file samples; {sum(s['backgroundActiveAtEnumeration'] is True for s in report['samples'])} overlapped background execution")
