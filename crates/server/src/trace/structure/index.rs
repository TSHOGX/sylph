use super::{
    Limits,
    parser::{self, Event, Span},
};
use sqlx::{Connection, Row, SqliteConnection, SqlitePool, sqlite::SqliteConnectOptions};
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

pub(crate) struct Index {
    pool: SqlitePool,
    max_query_steps: u64,
    #[cfg(test)]
    last_query_steps: AtomicU64,
    // Drop the pool before removing the scratch files. No source evidence lives here.
    _directory: tempfile::TempDir,
}
#[derive(Debug)]
pub(crate) struct Node {
    pub id: u64,
    pub parent: Option<u64>,
    pub ordinal: u64,
    pub kind: parser::Kind,
    pub value: Span,
    pub name: Option<Span>,
    pub children: u64,
}
fn node(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<Node> {
    Ok(Node {
        id: row.get::<i64, _>("id") as u64,
        parent: row
            .get::<Option<i64>, _>("parent")
            .map(|value| value as u64),
        ordinal: row.get::<i64, _>("ordinal") as u64,
        kind: serde_json::from_str(&row.get::<String, _>("kind"))?,
        value: Span {
            start: row.get::<i64, _>("start") as u64,
            end: row.get::<i64, _>("end") as u64,
        },
        name: row.get::<Option<i64>, _>("name_start").map(|start| Span {
            start: start as u64,
            end: row.get::<i64, _>("name_end") as u64,
        }),
        children: row.get::<i64, _>("children") as u64,
    })
}
impl Index {
    pub(crate) async fn build(
        directory: tempfile::TempDir,
        source: impl Read + Send + 'static,
        limits: Limits,
        cancel: Arc<AtomicBool>,
        progress: Arc<AtomicU64>,
        deadline: Instant,
    ) -> anyhow::Result<Self> {
        let max_query_steps = limits.max_query_steps;
        let options = SqliteConnectOptions::new()
            .filename(directory.path().join("nodes.sqlite3"))
            .create_if_missing(true)
            .pragma("journal_mode", "OFF")
            .pragma("synchronous", "OFF")
            .pragma("page_size", "4096")
            .pragma(
                "max_page_count",
                (limits.max_index_bytes / 4096).to_string(),
            )
            .pragma("cache_size", "-2048");
        let mut writer = SqliteConnection::connect_with(&options).await?;
        sqlx::query("CREATE TABLE nodes(id INTEGER PRIMARY KEY,parent INTEGER,ordinal INTEGER NOT NULL,kind TEXT NOT NULL,start INTEGER NOT NULL,end INTEGER,name_start INTEGER,name_end INTEGER,children INTEGER)").execute(&mut writer).await?;
        sqlx::query("CREATE UNIQUE INDEX child_order ON nodes(parent,ordinal)")
            .execute(&mut writer)
            .await?;
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut batch = Vec::with_capacity(128);
            parser::scan(source, limits.max_nodes, limits.max_depth, |event| {
                anyhow::ensure!(
                    !cancel.load(Ordering::Relaxed),
                    "Structured exploration cancelled"
                );
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "Structured exploration deadline exceeded"
                );
                if let Event::Start(node) = &event {
                    progress.store(node.id, Ordering::Relaxed);
                }
                batch.push(event);
                if batch.len() == 128 {
                    handle.block_on(flush(&mut writer, &mut batch))?;
                }
                Ok(())
            })?;
            handle.block_on(flush(&mut writer, &mut batch))?;
            // A malformed trailing suffix or cancellation never publishes this DB.
            anyhow::ensure!(
                !cancel.load(Ordering::Relaxed) && Instant::now() < deadline,
                "Structured exploration interrupted"
            );
            handle.block_on(writer.close())?;
            Ok(())
        })
        .await??;
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            // Cancellation must not lend a used VM budget to the next read.
            .after_release(|connection, _| {
                Box::pin(async move {
                    connection.lock_handle().await?.remove_progress_handler();
                    Ok(true)
                })
            })
            .connect_with(options.read_only(true).create_if_missing(false))
            .await?;
        Ok(Self {
            pool,
            max_query_steps,
            #[cfg(test)]
            last_query_steps: AtomicU64::new(0),
            _directory: directory,
        })
    }

    pub(crate) async fn node(&self, id: u64) -> anyhow::Result<Option<Node>> {
        sqlx::query("SELECT * FROM nodes WHERE id=?")
            .bind(i64::try_from(id)?)
            .fetch_optional(&self.pool)
            .await?
            .map(node)
            .transpose()
    }
    pub(crate) async fn children(
        &self,
        parent: u64,
        after: Option<u64>,
        limit: usize,
    ) -> anyhow::Result<Vec<Node>> {
        let mut connection = self.pool.acquire().await?;
        let steps = Arc::new(AtomicU64::new(0));
        let counter = steps.clone();
        let maximum = self.max_query_steps;
        connection
            .lock_handle()
            .await?
            .set_progress_handler(1, move || counter.fetch_add(1, Ordering::Relaxed) < maximum);
        let result = sqlx::query(
            "SELECT * FROM nodes WHERE parent=? AND ordinal>? ORDER BY ordinal LIMIT ?",
        )
        .bind(i64::try_from(parent)?)
        .bind(after.map(i64::try_from).transpose()?.unwrap_or(-1))
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *connection)
        .await;
        connection.lock_handle().await?.remove_progress_handler();
        #[cfg(test)]
        self.last_query_steps
            .store(steps.load(Ordering::Relaxed), Ordering::Relaxed);
        result?.into_iter().map(node).collect()
    }
}

async fn flush(writer: &mut SqliteConnection, events: &mut Vec<Event>) -> anyhow::Result<()> {
    let mut tx = writer.begin().await?;
    for event in events.drain(..) {
        match event {
            Event::Start(node) => {
                sqlx::query("INSERT INTO nodes(id,parent,ordinal,kind,start,name_start,name_end) VALUES(?,?,?,?,?,?,?)")
                    .bind(node.id as i64).bind(node.parent.map(|v|v as i64)).bind(node.ordinal as i64).bind(serde_json::to_string(&node.kind)?)
                    .bind(node.start as i64).bind(node.name.map(|span|span.start as i64)).bind(node.name.map(|span|span.end as i64))
                    .execute(&mut *tx).await?;
            }
            Event::End { id, end, children } => {
                sqlx::query("UPDATE nodes SET end=?,children=? WHERE id=?")
                    .bind(end as i64)
                    .bind(children as i64)
                    .bind(id as i64)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn indexed_child_pages_preserve_duplicates_and_skip_nested_subtrees() {
        let bytes = br#"{"x":[1,2],"x":{},"z":3}"#.to_vec();
        let index = Index::build(
            tempfile::tempdir().unwrap(),
            std::io::Cursor::new(bytes),
            super::super::limits(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Instant::now() + std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(index.node(1).await.unwrap().unwrap().children, 3);
        let page = index.children(1, None, 2).await.unwrap();
        assert_eq!(
            page.iter().map(|node| node.id).collect::<Vec<_>>(),
            vec![2, 5]
        );
        assert_eq!(page[0].children, 2);
        assert_eq!(page[1].children, 0);
        let next = index.children(1, Some(page[1].ordinal), 2).await.unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].id, 6);
    }

    #[tokio::test]
    async fn tenfold_sibling_growth_does_not_increase_late_page_scan_work() {
        let mut observed = vec![];
        for count in [1000, 10000] {
            let bytes = format!("[{}]", vec!["{\"nested\":[1,2,3]}"; count].join(",")).into_bytes();
            let limits = super::super::limits();
            let index = Index::build(
                tempfile::tempdir().unwrap(),
                std::io::Cursor::new(bytes),
                limits.clone(),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU64::new(0)),
                Instant::now() + std::time::Duration::from_secs(60),
            )
            .await
            .unwrap();
            let rows = index
                .children(
                    1,
                    Some(count as u64 - limits.page_items as u64 - 1),
                    limits.page_items,
                )
                .await
                .unwrap();
            assert_eq!(rows.len(), limits.page_items);
            let steps = index.last_query_steps.load(Ordering::Relaxed);
            assert!(steps > 0 && steps < limits.max_query_steps);
            observed.push(steps);
        }
        assert!(
            observed[1] <= observed[0] + 32,
            "late page scan grew with siblings: {observed:?}"
        );
    }

    #[tokio::test]
    async fn node_depth_disk_and_deadline_bounds_refuse_partial_indices() {
        let normal = super::super::limits();
        let cases = [
            (
                Limits {
                    max_nodes: 2,
                    ..normal.clone()
                },
                b"[1,2]".as_slice(),
                false,
            ),
            (
                Limits {
                    max_depth: 2,
                    ..normal.clone()
                },
                b"[[[]]]".as_slice(),
                false,
            ),
            (
                Limits {
                    max_index_bytes: 4096,
                    ..normal.clone()
                },
                b"[1]".as_slice(),
                false,
            ),
            (normal, b"[1]".as_slice(), true),
        ];
        for (limits, bytes, expired) in cases {
            let deadline = if expired {
                Instant::now()
            } else {
                Instant::now() + std::time::Duration::from_secs(3)
            };
            assert!(
                Index::build(
                    tempfile::tempdir().unwrap(),
                    std::io::Cursor::new(bytes.to_vec()),
                    limits,
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicU64::new(0)),
                    deadline
                )
                .await
                .is_err()
            );
        }
    }
}
