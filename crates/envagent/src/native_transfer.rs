//! Session-scoped temporary files bound native transfers independently of frame size.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use sylph_protocol::{NativeTransfer, NativeTransferChunk, SessionId, NATIVE_TRANSFER_CHUNK_BYTES};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

const TRANSFER_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_TRANSFERS: usize = 16;

struct Entry {
    session: SessionId,
    file: tempfile::NamedTempFile,
    touched: Instant,
    sealed: bool,
}

#[derive(Clone)]
pub(crate) struct Transfers {
    root: Arc<tempfile::TempDir>,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Transfers {
    pub fn new(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        Ok(Self {
            root: Arc::new(
                tempfile::Builder::new()
                    .prefix("native-")
                    .tempdir_in(root)?,
            ),
            entries: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn temporary_file(&self) -> Result<tempfile::NamedTempFile> {
        Ok(tempfile::NamedTempFile::new_in(self.root.path())?)
    }

    pub async fn insert(
        &self,
        session: &SessionId,
        file: tempfile::NamedTempFile,
        sealed: bool,
    ) -> Result<NativeTransfer> {
        let bytes = file.as_file().metadata()?.len();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, entry| entry.touched.elapsed() < TRANSFER_TTL);
        ensure!(
            entries.len() < MAX_TRANSFERS,
            "too many unfinished native transfers"
        );
        let id = uuid::Uuid::new_v4().to_string();
        entries.insert(
            id.clone(),
            Entry {
                session: session.clone(),
                file,
                touched: Instant::now(),
                sealed,
            },
        );
        Ok(NativeTransfer { id, bytes })
    }

    pub async fn append(
        &self,
        session: &SessionId,
        id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<NativeTransfer> {
        ensure!(
            bytes.len() <= NATIVE_TRANSFER_CHUNK_BYTES,
            "native transfer chunk is too large"
        );
        let mut entries = self.entries.lock().await;
        let entry = entries
            .get_mut(id)
            .context("native transfer is absent or expired")?;
        ensure!(
            &entry.session == session && entry.touched.elapsed() < TRANSFER_TTL,
            "native transfer does not belong to this session or expired"
        );
        ensure!(!entry.sealed, "native transfer is sealed");
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(entry.file.path())
            .await?;
        let len = file.metadata().await?.len();
        // A retried chunk is accepted only when its exact bytes are already present.
        ensure!(offset <= len, "native transfer offset has a gap");
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        if offset < len {
            ensure!(
                offset + bytes.len() as u64 <= len,
                "native transfer retry overlaps its tail"
            );
            let mut held = vec![0; bytes.len()];
            file.read_exact(&mut held).await?;
            ensure!(held == bytes, "native transfer retry changed bytes");
        } else {
            file.write_all(bytes).await?;
            file.sync_data().await?;
        }
        entry.touched = Instant::now();
        Ok(NativeTransfer {
            id: id.to_owned(),
            bytes: len.max(offset + bytes.len() as u64),
        })
    }

    pub async fn read(
        &self,
        session: &SessionId,
        id: &str,
        offset: u64,
    ) -> Result<NativeTransferChunk> {
        let mut entries = self.entries.lock().await;
        let entry = entries
            .get_mut(id)
            .context("native transfer is absent or expired")?;
        ensure!(
            &entry.session == session && entry.touched.elapsed() < TRANSFER_TTL,
            "native transfer does not belong to this session or expired"
        );
        ensure!(entry.sealed, "native transfer is incomplete");
        let mut file = tokio::fs::File::open(entry.file.path()).await?;
        let len = file.metadata().await?.len();
        ensure!(offset <= len, "native transfer offset exceeds its length");
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = vec![0; (len - offset).min(NATIVE_TRANSFER_CHUNK_BYTES as u64) as usize];
        file.read_exact(&mut bytes).await?;
        entry.touched = Instant::now();
        Ok(NativeTransferChunk {
            next_offset: offset + bytes.len() as u64,
            bytes,
        })
    }

    pub async fn take(&self, session: &SessionId, id: &str) -> Result<tempfile::NamedTempFile> {
        let mut entries = self.entries.lock().await;
        let entry = entries
            .get(id)
            .context("native transfer is absent or expired")?;
        ensure!(
            &entry.session == session && entry.touched.elapsed() < TRANSFER_TTL,
            "native transfer does not belong to this session or expired"
        );
        Ok(entries.remove(id).expect("entry checked under lock").file)
    }
}
