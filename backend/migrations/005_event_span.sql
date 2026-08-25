-- Delegated trajectories: which one an event happened in.
--
-- `docs/session-protocol.md` § Delegated trajectories. A `subagent` event's
-- `stream_id` is a trajectory id, and every event may name the trajectory it ran
-- in; NULL means the root turn. That one pointer expresses a subagent, a nested
-- subagent, and a dynamic workflow's parallel phases, because none of those is a
-- distinct case — they differ only in the shape of the resulting tree.
--
-- Nullable rather than defaulted to '': absent and "the root" are the same fact,
-- and a sentinel would make every root event carry a value that means nothing.
ALTER TABLE events ADD COLUMN span TEXT;

-- Reading one trajectory's own events without scanning the session. The timeline
-- builds the whole tree in one pass and does not need this, but focus mode and
-- replay of a single delegated subtree do, and it is what keeps a 20k-event
-- session's drill-down from being a full scan.
CREATE INDEX idx_events_span ON events(session_id, span, seq);
