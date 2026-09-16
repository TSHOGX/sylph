CREATE TABLE search_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),generation TEXT NOT NULL,evidence_cut INTEGER NOT NULL,view_revision INTEGER NOT NULL,ready INTEGER NOT NULL);
CREATE TABLE search_documents(id INTEGER PRIMARY KEY,document_key TEXT NOT NULL UNIQUE,environment TEXT NOT NULL,agent_ref TEXT NOT NULL,selection_ref TEXT,history_mode TEXT NOT NULL,representation TEXT NOT NULL,document_json TEXT NOT NULL);
CREATE INDEX search_documents_scope ON search_documents(environment,agent_ref,selection_ref,history_mode,representation);
CREATE VIRTUAL TABLE search_fts USING fts5(text,tokenize='jieba');
CREATE TABLE search_jobs(id TEXT PRIMARY KEY,subject TEXT NOT NULL,scope_hash TEXT NOT NULL,snapshot_json TEXT NOT NULL,trace_lease TEXT NOT NULL,expires_at_ms INTEGER NOT NULL,limited INTEGER NOT NULL,hits_json TEXT NOT NULL) WITHOUT ROWID;
CREATE INDEX search_job_expiry ON search_jobs(expires_at_ms,id);
