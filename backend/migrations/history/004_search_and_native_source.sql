-- Cross-session search, and the retention vocabulary that outlived the separate
-- archive.
--
-- Two changes that belong in one migration because both are consequences of
-- `docs/architecture.md` § One authority: with one store there is no second Sylph
-- copy to search *or* to wait for.
--
-- # `blocks_fts`
--
-- **Search is the one capability this rewrite takes from working to absent**, so
-- this is net-new work rather than a port: `/api/v1/search` was entirely
-- `tracedb.search()`, deleting the separate archive deleted the only
-- implementation, and nothing in this repo has ever held an FTS index.
--
-- **It cannot be an external-content FTS5 table**, which is the shape the separate
-- archive used (`content='events'`, `content_rowid='id'`) and therefore the obvious
-- thing to copy. External content re-reads the column from the base table at query
-- time, and `blocks.text_inline` is not text: `text_encoding` says it may be `zstd`,
-- and when the text is externalized to CAS the column is **NULL** and the bytes live
-- under `blobs/`. So an external-content index would index compressed bytes for some
-- rows and nothing at all for the rows most worth finding — the large ones.
-- Consolidation therefore inserts the *decoded* text, which costs a second copy of
-- it and is the price of the tier split (`docs/storage.md` § "`blocks_fts` is
-- standalone").
--
-- `session_id` and `seq` ride along `UNINDEXED` so a hit resolves to a block without
-- a join back through a rowid mapping that would have to be maintained separately.
--
-- **The tokenizer is `unicode61`, and that is a stated limitation for CJK rather
-- than a default nobody looked at.** `unicode61` splits on non-alphanumerics, so a
-- run of Han characters becomes one token and a query for a substring of it does not
-- match — which matters here, because this project's own transcripts are
-- substantially Chinese. The separate archive solved it with a loadable jieba
-- tokenizer, and that remains the reference implementation if segmented CJK search is
-- wanted. It is deliberately not adopted yet: a loadable SQLite extension is a
-- per-platform build artifact and an `sqlx` connection has to be told to allow
-- loading it, which is a real dependency to take on for a capability that is
-- currently zero. Ship the stock tokenizer; treat CJK segmentation as a separate
-- decision with a known answer.
CREATE VIRTUAL TABLE IF NOT EXISTS blocks_fts USING fts5(
  text,
  session_id UNINDEXED,
  seq        UNINDEXED,
  tokenize = 'unicode61'
);

-- # Retention provenance, restated for one store
--
-- `002_watermark_provenance.sql` added `archive_id` and `source_fingerprint`, and
-- both named things that no longer exist. `archive_id` held the separate archive's
-- own id for a session — the second id space the identity triple deletes — and
-- `source_fingerprint` held that archive's `file-v2:<sha256>:<bytes>:<mtime_ns>`
-- over a native file, read from a `show` response nothing answers any more.
--
-- What replaces them is narrower and answers the question the pair was reaching
-- for. `docs/storage.md` § Two retention classes: the only remaining downstream copy
-- is the **vendor's own native store**, a confirmation can only come from the
-- envagent holding it, and an unreachable environment means the question was never
-- put.
--
-- `withheld` is the load-bearing addition, and it exists because of a rule this repo
-- has already paid for: **"cannot ask" must never be recorded as "asked, and nothing
-- is safe".** A watermark row is a claim that a check happened. The caller used to
-- infer withholding from `confirmed_seq IS NULL`, which is exactly the conflation the
-- spec rejects — it cannot distinguish "the environment answered, and nothing is safe
-- yet" from "no envagent was connected, so nobody was asked", and those call for
-- opposite responses.
--
-- Both stay nullable: a terminal session has no native source to describe and
-- nothing to withhold for.
ALTER TABLE watermarks DROP COLUMN archive_id;
ALTER TABLE watermarks DROP COLUMN source_fingerprint;

-- What the environment reported about the native source, for the audit row.
-- 'present' | 'absent' | 'unreachable'.
ALTER TABLE watermarks ADD COLUMN source TEXT;
-- Why nothing was confirmed, when nothing was. NULL when something was.
ALTER TABLE watermarks ADD COLUMN withheld TEXT;
