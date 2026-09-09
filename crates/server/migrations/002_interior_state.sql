-- A trajectory's interior reachability is a durable fact, not a frontend inference from
-- whether one client happened to receive child events. Existing rows remain NULL and use
-- the compatibility fallback in the client.
ALTER TABLE events ADD COLUMN interior_state TEXT;
