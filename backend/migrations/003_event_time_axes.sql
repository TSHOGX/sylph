-- Make source time and server receipt time explicit without breaking the legacy
-- created_at_ms field. `recorded_at_ms` is NULL when the vendor supplied no usable
-- timestamp; `arrival_at_ms` is always the server's receipt clock.
ALTER TABLE events ADD COLUMN arrival_at_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE events ADD COLUMN recorded_at_ms INTEGER;
UPDATE events SET arrival_at_ms = created_at_ms WHERE arrival_at_ms = 0;
