#!/usr/bin/env python3
"""Falsify trace transaction choices in temporary SQLite databases, never Sylph data.

This exercises a small reference model, not the future production implementation.
--fault introduces one intentional broken promise; its run must exit nonzero.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile


def open_db(path):
    conn = sqlite3.connect(path)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=FULL")
    conn.execute("PRAGMA foreign_keys=ON")
    return conn


def require(condition, promise):
    if not condition:
        raise AssertionError(promise)


def evidence_occurrences(root, fault):
    path = root / "evidence.db"
    conn = open_db(path)
    conn.executescript("""
        CREATE TABLE observations (
          id INTEGER PRIMARY KEY, source TEXT, generation INTEGER, position INTEGER,
          digest TEXT, body BLOB, UNIQUE(source, generation, position));
        CREATE TABLE receipts (
          producer TEXT, sequence INTEGER, observation INTEGER REFERENCES observations(id),
          PRIMARY KEY(producer, sequence));
    """)

    def admit(producer, sequence, position, body):
        digest = hashlib.sha256(body).hexdigest()
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            receipt = conn.execute(
                "SELECT observation FROM receipts WHERE producer=? AND sequence=?",
                (producer, sequence),
            ).fetchone()
            if receipt:
                observed = conn.execute(
                    "SELECT position,body FROM observations WHERE id=?", receipt
                ).fetchone()
                require(observed == (position, body), "receipt collision is refused")
                return receipt[0]
            if fault == "content_dedup":
                prior = conn.execute(
                    "SELECT id,body FROM observations WHERE digest=?", (digest,)
                ).fetchone()
            else:
                prior = conn.execute(
                    "SELECT id,body FROM observations WHERE source='native' AND generation=1 AND position=?",
                    (position,),
                ).fetchone()
            if prior:
                require(prior[1] == body, "source-position collision is refused")
                observation = prior[0]
            else:
                observation = conn.execute(
                    "INSERT INTO observations(source,generation,position,digest,body) VALUES('native',1,?,?,?)",
                    (position, digest, body),
                ).lastrowid
            conn.execute("INSERT INTO receipts VALUES(?,?,?)", (producer, sequence, observation))
            return observation

    # Deliberately retain lexical JSON detail that a parsed value cannot reproduce.
    raw = b'{ "text": "repeat", "key": 1, "key": 2 }\r\n'
    first = admit("follow", 1, 10, raw)
    require(admit("follow", 1, 10, raw) == first, "redelivery retains its original observation")
    require(admit("reread", 1, 10, raw) == first, "reread recognizes source occurrence")
    second = admit("follow", 2, 20, raw)
    require(second != first, "equal bytes at different positions remain different occurrences")
    conn.close()  # Reopen at the boundary where the acknowledgment might have been lost.
    conn = open_db(path)
    require(conn.execute("SELECT COUNT(*) FROM observations").fetchone()[0] == 2,
            "two repeated occurrences survive reopening")
    require(conn.execute("SELECT body FROM observations WHERE id=?", (first,)).fetchone()[0] == raw,
            "lexical native bytes survive reopening")
    conn.close()
    return "source occurrences, redelivery, lexical bytes"


def operational_outbox(root, fault):
    control_path = root / "control.db"
    archive_path = root / "outbox-evidence.db"
    control = open_db(control_path)
    archive = open_db(archive_path)
    control.executescript("""
        CREATE TABLE commands(id TEXT PRIMARY KEY, dispatch_attempts INTEGER);
        CREATE TABLE outbox(id TEXT PRIMARY KEY, body BLOB, exported INTEGER);
    """)
    archive.execute("CREATE TABLE facts(id TEXT PRIMARY KEY, body BLOB)")
    with control:
        control.execute("INSERT INTO commands VALUES('command-1',1)")
        control.execute("INSERT INTO outbox VALUES('accepted-1',?,0)", (b'original submitted bytes',))
    control.close()  # Crash after command/outbox commit, before export.
    control = open_db(control_path)
    row = control.execute("SELECT id,body FROM outbox WHERE exported=0").fetchone()
    with archive:
        archive.execute("INSERT INTO facts VALUES(?,?)", row)
    archive.close()  # Crash after evidence commit, before outbox mark.
    archive = open_db(archive_path)
    with archive:
        archive.execute("INSERT OR IGNORE INTO facts VALUES(?,?)", row)
        require(archive.execute("SELECT body FROM facts WHERE id=?", (row[0],)).fetchone()[0] == row[1],
                "duplicate export has identical bytes")
    with control:
        if fault == "export_redispatch":
            control.execute("UPDATE commands SET dispatch_attempts=dispatch_attempts+1")
        control.execute("UPDATE outbox SET exported=1 WHERE id=?", (row[0],))
    require(archive.execute("SELECT COUNT(*) FROM facts").fetchone()[0] == 1,
            "outbox retry creates one trace fact")
    require(control.execute("SELECT dispatch_attempts FROM commands").fetchone()[0] == 1,
            "outbox export never redispatches a command")
    control.close()
    archive.close()
    return "control outbox crash boundaries without command redispatch"


def temporal_pages(root, fault):
    conn = open_db(root / "temporal.db")
    conn.executescript("""
        CREATE TABLE body(revision TEXT PRIMARY KEY, bytes BLOB);
        CREATE TABLE summaries(
          ref TEXT, since INTEGER, until INTEGER, rank INTEGER, body TEXT REFERENCES body(revision),
          PRIMARY KEY(ref,since));
        CREATE INDEX snapshot_order ON summaries(rank,ref,since,until);
    """)
    with conn:
        for rank, ref in enumerate(("a", "b", "c"), 1):
            conn.execute("INSERT INTO body VALUES(?,?)", (ref + "1", (ref + " original").encode()))
            conn.execute("INSERT INTO summaries VALUES(?,1,NULL,?,?)", (ref, rank * 10, ref + "1"))

    def page(revision, after=0):
        return conn.execute("""
            SELECT ref,rank,body FROM summaries
            WHERE since<=? AND (until IS NULL OR until>?) AND rank>?
            ORDER BY rank,ref LIMIT 2
        """, (revision, revision, after)).fetchall()

    first = page(1)
    require([r[0] for r in first] == ["a", "b"], "initial bounded page is ordered")
    with conn:
        conn.execute("UPDATE summaries SET until=2 WHERE ref IN ('b','c')")
        conn.execute("INSERT INTO body VALUES('b2',?)", (b'b corrected',))
        conn.execute("INSERT INTO summaries VALUES('b',2,NULL,20,'b2')")
        conn.execute("INSERT INTO summaries VALUES('c',2,NULL,5,'c1')")
    second = page(2 if fault == "mutable_page" else 1, first[-1][1])
    require([r[0] for r in first + second] == ["a", "b", "c"],
            "late move cannot omit or duplicate a snapshot page member")
    require(conn.execute("SELECT bytes FROM body WHERE revision=?", (first[-1][2],)).fetchone()[0] == b'b original',
            "old page body remains the same immutable revision")
    require([r[0] for r in page(2)] == ["c", "a"], "new snapshot sees the corrected order")
    conn.close()
    return "snapshot pagination and immutable bodies after late reorder"


def generation_publication(root, fault):
    catalog_path = root / "catalog.db"
    catalog = open_db(catalog_path)
    catalog.executescript("""
        CREATE TABLE admitted(seq INTEGER PRIMARY KEY, value TEXT);
        CREATE TABLE active(singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation TEXT, cut INTEGER);
        INSERT INTO active VALUES(1,'old',2);
        INSERT INTO admitted VALUES(1,'a'),(2,'b'),(3,'c');
    """)

    def generation(name, cut):
        conn = open_db(root / (name + ".db"))
        conn.executescript("CREATE TABLE objects(seq INTEGER PRIMARY KEY,value TEXT); CREATE TABLE state(cut INTEGER);")
        with conn:
            conn.executemany("INSERT INTO objects VALUES(?,?)", catalog.execute(
                "SELECT seq,value FROM admitted WHERE seq<=?", (cut,)).fetchall())
            conn.execute("INSERT INTO state VALUES(?)", (cut,))
        return conn

    old = generation("old", 2)
    new = generation("new", 1)
    if fault == "early_publish":
        with catalog:
            catalog.execute("UPDATE active SET generation='new',cut=3")
    catalog.close()  # Crash while the private build is incomplete.
    catalog = open_db(catalog_path)
    require(catalog.execute("SELECT generation,cut FROM active").fetchone() == ("old", 2),
            "incomplete generation never becomes readable")
    # A drained writer barrier fixes old at cut 2 and private generation at cut 3.
    with new:
        new.executemany("INSERT INTO objects VALUES(?,?)", catalog.execute(
            "SELECT seq,value FROM admitted WHERE seq>1 AND seq<=3").fetchall())
        new.execute("UPDATE state SET cut=3")
    require(new.execute("SELECT COUNT(*) FROM objects").fetchone()[0] == 3,
            "new generation is complete at its publication cut")
    new.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    new.close()
    with catalog:  # Arrival continues while projection publication is fenced.
        catalog.execute("INSERT INTO admitted VALUES(4,'d')")
    with catalog:
        changed = catalog.execute(
            "UPDATE active SET generation='new',cut=3 WHERE generation='old' AND cut=2"
        ).rowcount
        require(changed == 1, "publication compare-and-swap names its prior generation")
    catalog.close()  # Crash after pointer commit, before routing/worker resumption.
    catalog = open_db(catalog_path)
    require(catalog.execute("SELECT generation,cut FROM active").fetchone() == ("new", 3),
            "reopening uses the committed generation pointer")
    require(old.execute("SELECT COUNT(*) FROM objects").fetchone()[0] == 2,
            "leased old generation remains independently readable")
    new = open_db(root / "new.db")
    start = 5 if fault == "skip_tail" else new.execute("SELECT cut+1 FROM state").fetchone()[0]
    with new:
        new.executemany("INSERT INTO objects VALUES(?,?)", catalog.execute(
            "SELECT seq,value FROM admitted WHERE seq>=? ORDER BY seq", (start,)).fetchall())
        new.execute("UPDATE state SET cut=4")
    require(new.execute("SELECT seq,value FROM objects ORDER BY seq").fetchall() ==
            catalog.execute("SELECT seq,value FROM admitted ORDER BY seq").fetchall(),
            "arrivals after the publication cut remain in the resumed projection")
    old.close()
    new.close()
    catalog.close()
    return "private generation, atomic pointer, old lease and admitted tail"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fault", choices=("content_dedup", "export_redispatch", "mutable_page", "early_publish", "skip_tail"))
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="sylph-trace-design-") as directory:
        root = Path(directory)
        results = [case(root, args.fault) for case in (
            evidence_occurrences, operational_outbox, temporal_pages, generation_publication
        )]
    print(json.dumps({"status": "passed", "sqlite_version": sqlite3.sqlite_version,
                      "scope": "disposable reference model; not production or power-loss certification",
                      "checks": results}, indent=2))


if __name__ == "__main__":
    main()
