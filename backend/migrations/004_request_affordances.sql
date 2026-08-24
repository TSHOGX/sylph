-- Two facts about a request that a client needs in order to render it honestly.
--
-- Added as a migration rather than folded into 002 because sqlx checksums each
-- file: editing 002 in place would make every existing journal refuse to open.
--
-- `allow_persistent` is not cosmetic. An agent can mark an ask where granting
-- "always" would write a permission rule *broader than the ask itself* — Claude's
-- own schema says a host "should omit any persistent-rule affordance when set".
-- Offering it anyway converts a narrow yes into a wide one the user never read, so
-- the flag has to survive a restart along with the request it qualifies.
--
-- Defaults to 1 so every row already stored keeps the behaviour it was created
-- under, and an adapter that never sets it stays correct.
ALTER TABLE pending_requests
  ADD COLUMN allow_persistent INTEGER NOT NULL DEFAULT 1;

-- When the agent stops waiting, if it will. Advisory and never acted on here: the
-- server must not auto-apply a default when it lapses, because that would answer
-- on the user's behalf in exactly the decision this channel exists to give them.
-- `pi` resolves its own dialogs on timeout, so a client with no deadline to show is
-- racing a clock it cannot see.
ALTER TABLE pending_requests
  ADD COLUMN expires_at_ms INTEGER;
