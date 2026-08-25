-- The input a request declares it wants collected.
--
-- A new file rather than an edit to 002 or 004 for the reason 004 already records:
-- sqlx checksums each migration, so editing one in place makes every existing
-- journal refuse to open.
--
-- Nullable, and absent is meaningful rather than missing: a request with no form is
-- an approval, whose subject is one proposed tool input that a client renders from
-- `detail_json`. Only a request that declares a form gets one rendered, which is
-- what keeps every already-stored row correct under the new code.
--
-- Stored rather than held on the link because a pending request is durable state: the
-- dialog may be rendered by a client that attached long after the request was raised,
-- and it cannot be answered without knowing what was asked for.
ALTER TABLE pending_requests
  ADD COLUMN form_json TEXT;
