-- Explicit source and server clocks for consolidated blocks. Existing rows predate
-- the split; their legacy created_at_ms is the only available timestamp, so it is
-- retained as the compatibility value and copied to arrival_at_ms.
ALTER TABLE blocks ADD COLUMN arrival_at_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE blocks ADD COLUMN recorded_at_ms INTEGER;
UPDATE blocks SET arrival_at_ms = created_at_ms WHERE arrival_at_ms = 0;
