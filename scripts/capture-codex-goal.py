#!/usr/bin/env python3
"""Retain one native goal row and its schema in a read-only SQLite transaction."""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import struct

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("thread_id")
parser.add_argument("output", type=Path)
parser.add_argument("--database", type=Path, default=Path.home() / ".codex/goals_1.sqlite")
args = parser.parse_args()
database = args.database.resolve(strict=True)
connection = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)
connection.text_factory = bytes
connection.execute("PRAGMA query_only=ON")
connection.execute("BEGIN")
try:
    schema = connection.execute("PRAGMA table_info(thread_goals)").fetchall()
    if not 0 < len(schema) <= 256:
        raise ValueError("Goal schema is absent or exceeds the capture column limit")
    columns = [{"name": row[1].decode(), "declaredType": row[2].decode()} for row in schema]
    quoted = ['"' + column["name"].replace('"', '""') + '"' for column in columns]
    size = connection.execute(
        "SELECT " + "+".join(f"coalesce(length(CAST({name} AS BLOB)),0)" for name in quoted)
        + " FROM thread_goals WHERE thread_id=? LIMIT 1", (args.thread_id,),
    ).fetchone()
    if size is None:
        raise ValueError("The requested thread has no native goal row")
    if not 0 <= size[0] <= 8 * 1024 * 1024:
        raise ValueError("Goal row exceeds the capture byte limit")
    expressions = quoted + [f"typeof({name})" for name in quoted]
    row = connection.execute(
        "SELECT " + ",".join(expressions) + " FROM thread_goals WHERE thread_id=? LIMIT 1",
        (args.thread_id,),
    ).fetchone()
    if row is None:
        raise ValueError("The requested thread has no native goal row")
    values = {}
    for index, column in enumerate(columns):
        value, kind = row[index], row[index + len(columns)].decode()
        if kind == "null":
            encoded = {"kind": "null"}
        elif kind == "integer":
            encoded = {"kind": "integer", "value": value}
        elif kind == "real":
            encoded = {"kind": "real_bits", "value": struct.unpack(">Q", struct.pack(">d", value))[0]}
        elif kind == "blob":
            encoded = {"kind": "blob", "value": list(value)}
        elif kind == "text":
            try:
                encoded = {"kind": "text", "value": value.decode("utf-8")}
            except UnicodeDecodeError:
                encoded = {"kind": "text_bytes", "value": list(value)}
        else:
            raise ValueError(f"Unsupported SQLite storage class: {kind}")
        values[column["name"]] = encoded
    create_sql = connection.execute("SELECT sql FROM sqlite_master WHERE type='table' AND name='thread_goals'").fetchone()[0]
    snapshot = {
        "databaseLocator": str(database), "table": "thread_goals", "columns": columns,
        "status": {"kind": "available"}, "row": values, "selectedSource": None,
        "selectedGeneration": None, "selectedRecordCount": None, "selectedPendingBytes": None,
    }
finally:
    connection.rollback()
    connection.close()

def encode(value):
    return (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode()

source = encode(snapshot)
snapshot["databaseLocator"] = "/vendor/codex/goals_1.sqlite"
retained = encode(snapshot)
args.output.mkdir(parents=True, exist_ok=True)
(args.output / "goal.json").write_bytes(retained)
(args.output / "schema.sql").write_bytes(create_sql + b";\n")
(args.output / "capture.json").write_text(json.dumps({
    "method": "scripts/capture-codex-goal.py: one read-only transaction reads the actual thread_goals schema, every selected column and every SQLite storage class, including committed WAL data. No model or goal mutation is requested.",
    "limits": "This is a logical database-row snapshot, not a copy or hash of SQLite file pages. The producing binary version is not exposed by this row and is not inferred from another installed CLI. Only databaseLocator is substituted; all row values and identities are retained.",
    "source_snapshot_sha256": hashlib.sha256(source).hexdigest(),
    "retained_sha256": hashlib.sha256(retained).hexdigest(),
    "schema_sha256": hashlib.sha256(create_sql + b";\n").hexdigest(),
    "thread_id": args.thread_id,
}, indent=2) + "\n")
print(args.output)
