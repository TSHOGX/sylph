#!/usr/bin/env python3
"""Collect the production-path storage measurements printed by Rust regressions.

Usage: python3 collect-storage-timing.py LABEL LOG [LOG ...] > REPORT.json
Run the timeline_nodes and storage test binaries with --nocapture first. The label
identifies the measured checkout; baseline and fixed logs must not be mixed.
"""
import datetime
import json
import pathlib
import re
import sys

prefixes = {
    "fold_measurement": "folds",
    "timeline_measurement": "firstScreen",
    "timeline_patch_measurement": "projectionPatches",
    "timeline_file_lock_window": "heldHistoryWriter",
    "lazy_body_measurement": "lazyBodies",
    "mixed_storage_measurement": "mixedWorkloads",
}
report = {
    "label": sys.argv[1],
    "collectedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "profile": "Rust test profile, unoptimized with debuginfo",
    "commands": [
        "cargo test -p sylph-server --test timeline_nodes -- --nocapture",
        "cargo test -p sylph-server --test storage folding_ten_times_more -- --nocapture",
    ],
    "measurements": {},
    "limits": [
        "Synthetic records in disposable databases; no vendor files or model turns.",
        "Authenticated router and envagent bridge timings exclude browser layout and HTTP sockets.",
        "Wall-clock values depend on concurrent load; VM steps measure query and projection work.",
        "coldUs names the first request after seeding, not an OS cold-cache measurement.",
        "GC may defer while a publisher owns the shared publication gate; sweepPasses reports retries.",
    ],
}
for filename in sys.argv[2:]:
    for line in pathlib.Path(filename).read_text().splitlines():
        prefix, _, payload = line.partition(" ")
        if prefix in prefixes:
            value = json.loads(payload)
            if prefix == "mixed_storage_measurement":
                samples = sorted(sample["durationUs"] for sample in value["files"])
                value["fileSummary"] = {
                    "samples": len(samples),
                    "allWritersActiveSamples": sum(sample["allWritersActive"] for sample in value["files"]),
                    "medianUs": samples[len(samples) // 2],
                    "p95Us": samples[min(len(samples) - 1, int(len(samples) * 0.95))],
                    "maximumUs": samples[-1],
                }
            report["measurements"].setdefault(prefixes[prefix], []).append(value)
        match = re.fullmatch(r"timeline_query_steps small=(\d+) large=(\d+)", line)
        if match:
            report["measurements"].setdefault("childQueryVmSteps", []).append({
                "small": int(match[1]), "large": int(match[2]),
            })
if not report["measurements"]:
    raise SystemExit("No measurement records found; use --nocapture and inspect the test result.")
json.dump(report, sys.stdout, indent=2)
print()
