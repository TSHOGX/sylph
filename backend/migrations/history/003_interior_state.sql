-- Preserve the latest producer-authored trajectory interior state through consolidation.
ALTER TABLE blocks ADD COLUMN interior_state TEXT;
