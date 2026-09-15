//! Explicit session search across current logical bodies and archived FTS rows.
use super::timeline::{body_snapshot, get};
use crate::error::{Error, Result};
use std::collections::HashSet;

impl super::JournalStore {
    /// Search explicit current bodies without adding whole-stream FTS rewrites to
    /// the token-rate write path. Only nodes with hot fragments are materialized;
    /// cold history keeps its existing FTS index. Both indexes use jieba phrases.
    pub async fn search_nodes(
        &self,
        history: &crate::storage::HistoryStore,
        session: &sylph_protocol::SessionId,
        query: &str,
    ) -> Result<Vec<(i32, String)>> {
        if query.trim().chars().count() < 2 {
            return Ok(Vec::new());
        }
        let mut tx = self.readers.begin().await?;
        let ids: Vec<(String, i32)> = sqlx::query_as("SELECT m.node_id,MAX(e.seq) FROM events e JOIN timeline_event_nodes m ON m.session_id=? AND m.seq=e.seq WHERE e.session_env=? AND e.session_agent=? AND e.session_native=? GROUP BY m.node_id")
            .bind(session.encode()).bind(session.environment.as_str()).bind(session.agent.as_str()).bind(session.native_id.as_str()).fetch_all(&mut *tx).await?;
        let mut hot = Vec::new();
        let mut seen = HashSet::new();
        for (id, seq) in ids {
            let (mut node, events, seqs) = body_snapshot(&mut tx, session, &id).await?;
            if let Some(turn) = id.strip_prefix("p:") {
                node.event.text = sqlx::query_scalar("SELECT request_text FROM turns WHERE id=?")
                    .bind(turn)
                    .fetch_one(&mut *tx)
                    .await?;
                node.event.body_ref = None;
            }
            let report = if let Some(event) = &node.trajectory {
                let mut report = node.clone();
                report.event = event.clone();
                let events = sqlx::query_as::<_, crate::models::Event>("SELECT * FROM events WHERE session_env=? AND session_agent=? AND session_native=? AND seq=?")
                    .bind(session.environment.as_str()).bind(session.agent.as_str()).bind(session.native_id.as_str()).bind(event.seq).fetch_all(&mut *tx).await?;
                Some((report, events, vec![event.seq]))
            } else {
                None
            };
            seen.insert(id.clone());
            hot.push((
                id,
                if node.event.seq < 0 {
                    seq
                } else {
                    node.event.seq
                },
                node,
                events,
                seqs,
                report,
            ));
        }
        // Ranked pagination needs one history snapshot as publication can change
        // corpus ranks between pages. Acquire it only after the journal snapshot.
        let mut archive = history
            .search_snapshot()
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        let mut cold_hits = Vec::new();
        let mut offset = 0;
        while cold_hits.len() < 20 {
            // Journal snapshot first, history second: publication before eviction
            // guarantees an archive cannot create a search gap. Filter before the
            // final limit, including stale block revisions and hot/cold duplicates.
            let page = crate::storage::HistoryStore::search_session_page(
                &mut archive,
                &session.encode(),
                query,
                40,
                offset,
            )
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
            let complete = page.len() < 40;
            for (seq, excerpt) in page {
                let node: Option<String> = sqlx::query_scalar(
                    "SELECT node_id FROM timeline_event_nodes WHERE session_id=? AND seq=?",
                )
                .bind(session.encode())
                .bind(seq)
                .fetch_optional(&mut *tx)
                .await?;
                let Some(id) = node else { continue };
                let Some(node) = get(&mut tx, &session.encode(), &id).await? else {
                    continue;
                };
                if node.event.seq >= 0
                    && (seq < node.body_start_seq || seq > node.event.seq)
                    && node
                        .trajectory
                        .as_ref()
                        .is_none_or(|report| report.seq != seq)
                {
                    continue;
                }
                if seen.insert(id) {
                    cold_hits.push((seq, excerpt));
                }
                if cold_hits.len() == 20 {
                    break;
                }
            }
            if complete {
                break;
            }
            offset += 40;
        }
        archive.commit().await?;
        tx.commit().await?;
        // Release the shared read pool before decoding full bodies or building
        // the query index. Explicit searches must not occupy interactive readers
        // while CPU work or CAS I/O runs.
        if hot.is_empty() {
            return Ok(cold_hits);
        }
        // A private, disposable index belongs to this explicit query. Do not put
        // it on the journal writer or scan bounded previews: either choice makes
        // large streaming bodies impose a hidden cost or lose matches.
        let index = crate::storage::sqlite::pool(
            sqlx::sqlite::SqliteConnectOptions::new().in_memory(true),
            1,
            false,
            true,
            None,
        )
        .await?;
        sqlx::query(
            "CREATE VIRTUAL TABLE search USING fts5(text, seq UNINDEXED, tokenize='jieba')",
        )
        .execute(&index)
        .await?;
        for (_, seq, node, events, seqs, report) in hot {
            let body =
                crate::storage::node_body::resolve(history, session, &node, events, seqs).await?;
            let mut text = body["text"].as_str().unwrap_or_default().to_owned();
            if let Some((report, events, seqs)) = report {
                let body =
                    crate::storage::node_body::resolve(history, session, &report, events, seqs)
                        .await?;
                text.push('\n');
                text.push_str(body["text"].as_str().unwrap_or_default());
            }
            sqlx::query("INSERT INTO search(text,seq) VALUES (?,?)")
                .bind(text)
                .bind(seq)
                .execute(&index)
                .await?;
        }
        let mut hits: Vec<(i32,String)> = sqlx::query_as("SELECT seq,snippet(search,0,'','','…',24) FROM search WHERE search MATCH ? ORDER BY rank LIMIT 20")
            .bind(crate::storage::history::fts5_phrase(query)).fetch_all(&index).await?;
        index.close().await;
        hits.extend(cold_hits);
        hits.truncate(20);
        Ok(hits)
    }
}
