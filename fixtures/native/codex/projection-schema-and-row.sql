-- codex `thread_history` projection: schema plus one real row.
--
-- Measured 2026-08-28 against codex-cli 0.150.1, ~/.codex/thread_history_1.sqlite.
--
-- Checked in as TEXT rather than as a database, deliberately: this table is a
-- pagination index *over* the rollouts, never a replacement for them. On this
-- install it holds 213 turn rows across 73 distinct threads, against 1258
-- rollout-*.jsonl files -- so a parser reading only SQLite would silently miss
-- 94% of the sessions on the machine. The JSONL is authoritative for `adopt`
-- and `watch`.
--
-- What the projection is nevertheless the ONLY carrier of, which is why an
-- `enumerate` implementation may prefer it where a thread is present:
-- turn-level `status`, `duration_ms`, `started_at`, `completed_at`. A rollout
-- scan would have to reconstruct all four.

-- ==== schema ====

CREATE TABLE _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
);

CREATE TABLE thread_history_projection_state (
    thread_id TEXT PRIMARY KEY,
    next_rollout_byte_offset INTEGER NOT NULL,
    next_rollout_ordinal INTEGER NOT NULL
);

CREATE TABLE thread_items (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    item_json TEXT NOT NULL, item_type TEXT NOT NULL DEFAULT '', updated_at_ordinal INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (thread_id, turn_id, item_id)
);

CREATE TABLE thread_turns (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    error_json TEXT,
    started_at INTEGER,
    completed_at INTEGER,
    duration_ms INTEGER,
    first_user_item_id TEXT,
    final_agent_item_id TEXT, rollout_byte_offset INTEGER, rollout_end_ordinal INTEGER, rollout_end_byte_offset INTEGER,
    PRIMARY KEY (thread_id, turn_id)
);

CREATE INDEX idx_thread_items_by_turn_page
    ON thread_items(thread_id, turn_id, rollout_ordinal);

CREATE INDEX idx_thread_items_by_turn_updated_page
    ON thread_items(thread_id, turn_id, updated_at_ordinal);

CREATE UNIQUE INDEX idx_thread_items_page
    ON thread_items(thread_id, rollout_ordinal);

CREATE INDEX idx_thread_items_updated_page
    ON thread_items(thread_id, updated_at_ordinal);

CREATE INDEX idx_thread_items_user_messages
    ON thread_items(thread_id, rollout_ordinal)
    WHERE item_type = 'userMessage';

CREATE UNIQUE INDEX idx_thread_turns_page
    ON thread_turns(thread_id, rollout_ordinal);


-- ==== one real thread_turns row ====
--
-- The offsets in this row are live, and that is the load-bearing fact:
-- `rollout_byte_offset` 18634 lands exactly on a line boundary in the matching
-- rollout (the preceding byte is a newline) and the record starting there is
-- `event_msg/task_started`; `rollout_end_byte_offset` 312831 equals that
-- file's size exactly. Matching rollout:
--   ~/.codex/sessions/2026/08/26/rollout-2026-08-26T10-59-39-01a03c02-4d90-7063-abdb-ebe7f821b74b.jsonl

INSERT INTO thread_turns (thread_id, turn_id, rollout_ordinal, status, error_json, started_at, completed_at, duration_ms, first_user_item_id, final_agent_item_id, rollout_byte_offset, rollout_end_ordinal, rollout_end_byte_offset)
VALUES ('01a03c02-4d90-7063-abdb-ebe7f821b74b', '01a03c02-db15-7873-9baa-0941a6c713eb', 1, 'completed', NULL, 1787713215, 1787713318, 103012, '01a03c02-db91-72b3-b114-21c860141cf6', 'msg_019dd1157feb0652016a8e57203f9487d1acdc5165c980df20', 18634, 61, 312831);


-- ==== turn status is a closed set on this install ====
--   completed    171
--   interrupted  23
--   failed       19

-- ==== projection state: how the index tracks its position in each rollout ====
-- `next_rollout_byte_offset` / `next_rollout_ordinal` are what make this an
-- incrementally-built index rather than a snapshot.
--   01a018fb-1f6f-7f22-8aa4-c88d8db92f7c  byte=24549718 ordinal=7026
--   01a015c3-0a65-7a93-bfd6-cb399dfb79b3  byte=15808158 ordinal=4171
--   01a01870-4ee9-79a0-9562-ff50ce65246e  byte=12785464 ordinal=2569
