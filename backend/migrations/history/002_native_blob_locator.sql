-- Large native sidecars are retained as deferred resources.  The locator is opaque to the
-- server: a local adapter may use a validated path, while a future envagent can replace it
-- with a machine-local handle without changing the native record key.
ALTER TABLE native_blobs ADD COLUMN locator TEXT;
