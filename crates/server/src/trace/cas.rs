//! Immutable physical CAS bytes with BLAKE3 verification and durable publication.
//! Logical payload types and manifest interpretation are owned by the trace evidence store.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;

/// A content-addressed store rooted at one directory.
#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where a hash lives. Two levels of two hex characters, so a store holding a
    /// million objects has ~256 directories of ~4000 rather than one of a million
    /// — which is where `readdir` and some filesystems degrade badly.
    fn path_for(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("invalid blob hash: {hash}");
        }
        Ok(self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash))
    }

    /// Store bytes and return their hash.
    ///
    /// Idempotent by construction: the same content yields the same path, and an
    /// existing object is left alone rather than rewritten. Skipping the rewrite
    /// is what makes storing a file an agent reads repeatedly nearly free.
    pub async fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let path = self.path_for(&hash)?;

        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let existing = tokio::fs::read(&path)
                .await
                .with_context(|| format!("Could not verify existing blob {:?}", path))?;
            if blake3::hash(&existing).to_hex().as_str() != hash {
                anyhow::bail!("blob hash mismatch for {:?}", path);
            }
            sync_parent_directories(&path).await?;
            return Ok(hash);
        }

        let parent = path
            .parent()
            .context("A blob path always has a parent directory")?;
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Could not create {:?}", parent))?;

        // Every writer owns its temporary file. Sharing one name lets a later
        // writer truncate the inode an earlier writer is about to publish.
        let temporary = parent.join(format!("{}.{}.partial", hash, uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .with_context(|| format!("Could not create {:?}", temporary))?;
        let result = async {
            file.write_all(bytes).await?;
            // Durable before the rename: a rename that lands before the data would
            // survive a power loss as a valid-looking empty object.
            file.sync_all().await?;
            drop(file);

            match tokio::fs::rename(&temporary, &path).await {
                Ok(()) => Ok(()),
                // Another writer may have published the same hash while we were
                // writing. Verify its complete object before treating this as
                // successful deduplication.
                Err(error) if tokio::fs::try_exists(&path).await.unwrap_or(false) => {
                    let existing = tokio::fs::read(&path).await?;
                    if blake3::hash(&existing).to_hex().as_str() == hash {
                        Ok(())
                    } else {
                        Err(error)
                    }
                }
                Err(error) => Err(error),
            }
        }
        .await;

        // Best-effort cleanup on every path. A failed write must not leave a
        // partial file that looks like a future object to diagnostics or sweeps.
        if let Err(error) = tokio::fs::remove_file(&temporary).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error).with_context(|| format!("Could not clean up {:?}", temporary));
            }
        }
        result.with_context(|| format!("Could not publish {:?}", path))?;
        sync_parent_directories(&path).await?;

        Ok(hash)
    }

    /// Read an object back. `None` when it is absent.
    pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        match tokio::fs::read(self.path_for(hash)?).await {
            Ok(bytes) => {
                if blake3::hash(&bytes).to_hex().as_str() != hash {
                    anyhow::bail!("blob hash mismatch for {hash}");
                }
                Ok(Some(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("Could not read a blob"),
        }
    }

    /// Delete an object. Callers must have established that nothing references it.
    ///
    /// Reachability is the refcount table's business, not this type's: an object
    /// shared by a pruned block and a retained one must survive, and only the
    /// index knows that.
    pub async fn remove(&self, hash: &str) -> Result<()> {
        let path = self.path_for(hash)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => sync_parent_directories(&path).await,
            // Already gone is the desired end state, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("Could not remove a blob"),
        }
    }

    pub(crate) async fn size(&self, hash: &str) -> Result<Option<u64>> {
        match tokio::fs::metadata(self.path_for(hash)?).await {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("Could not measure a blob"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

async fn sync_parent_directories(path: &Path) -> Result<()> {
    // The object fsync protects its bytes; directory fsyncs protect the rename and
    // newly-created shard paths. Repeat on dedup too: a concurrent writer may have
    // published the object but not yet made its directory entries durable.
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("Could not resolve blob directory")?
            .join(path)
    };
    tokio::task::spawn_blocking(move || {
        for directory in path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::File::open(directory)
                .and_then(|file| file.sync_all())
                .with_context(|| format!("Could not sync blob directory {:?}", directory))?;
        }
        Ok(())
    })
    .await
    .context("Blob directory sync task failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (BlobStore, TempDir) {
        let dir = TempDir::new().unwrap();
        (BlobStore::new(dir.path().join("cas")), dir)
    }

    #[tokio::test]
    async fn a_blob_round_trips_by_its_hash() {
        let (blobs, _dir) = store();
        let hash = blobs.put(b"hello world").await.unwrap();
        assert_eq!(
            blobs.get(&hash).await.unwrap().as_deref(),
            Some(&b"hello world"[..])
        );
    }

    /// Concurrent writers for one content hash must never corrupt the published
    /// object or leave temporary files behind. This exercises the same race that
    /// consolidation can hit when two turns externalize an identical payload.
    #[tokio::test]
    async fn concurrent_writes_of_one_hash_publish_one_intact_object() {
        let (blobs, _dir) = store();
        let bytes = vec![b'k'; 8192];
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let blobs = blobs.clone();
            let bytes = bytes.clone();
            tasks.push(tokio::spawn(
                async move { blobs.put(&bytes).await.unwrap() },
            ));
        }
        let mut hashes = Vec::new();
        for task in tasks {
            hashes.push(task.await.unwrap());
        }
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            blobs.get(&hashes[0]).await.unwrap().as_deref(),
            Some(bytes.as_slice())
        );
        assert!(walkdir(blobs.root())
            .iter()
            .all(|path| !path.to_string_lossy().contains("partial")));
    }

    /// The dedup property, which is what makes an agent re-reading one file cheap.
    #[tokio::test]
    async fn identical_content_stores_once_under_one_hash() {
        let (blobs, _dir) = store();
        let first = blobs.put(b"the same bytes").await.unwrap();
        let second = blobs.put(b"the same bytes").await.unwrap();
        assert_eq!(first, second);

        let mut files = Vec::new();
        for entry in walkdir(blobs.root()) {
            files.push(entry);
        }
        assert_eq!(files.len(), 1, "one object on disk, not two");
    }

    #[tokio::test]
    async fn different_content_gets_different_hashes() {
        let (blobs, _dir) = store();
        let a = blobs.put(b"one").await.unwrap();
        let b = blobs.put(b"two").await.unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn a_missing_blob_reads_as_none_rather_than_an_error() {
        let (blobs, _dir) = store();
        let absent = "0".repeat(64);
        assert!(blobs.get(&absent).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_tampered_blob_is_rejected_by_hash() {
        let (blobs, _dir) = store();
        let hash = blobs.put(b"original").await.unwrap();
        let path = blobs.root().join(&hash[0..2]).join(&hash[2..4]).join(&hash);
        tokio::fs::write(path, b"tampered").await.unwrap();
        let error = blobs.get(&hash).await.unwrap_err();
        assert!(format!("{error:#}").contains("hash mismatch"));
    }

    #[tokio::test]
    async fn malformed_hashes_are_errors_instead_of_paths_or_panics() {
        let (blobs, _dir) = store();
        for hash in ["".to_string(), "a".repeat(63), "z".repeat(64)] {
            assert!(blobs.get(&hash).await.is_err());
            assert!(blobs.remove(&hash).await.is_err());
        }
    }

    /// Removing an object that is already gone is the desired end state.
    #[tokio::test]
    async fn removing_an_absent_blob_is_not_an_error() {
        let (blobs, _dir) = store();
        assert!(blobs.remove(&"a".repeat(64)).await.is_ok());
    }

    /// No `.partial` file may survive a completed write, or a later `put` of the
    /// same content would find a truncated object at the right hash and trust it.
    #[tokio::test]
    async fn a_completed_write_leaves_no_temporary_file_behind() {
        let (blobs, _dir) = store();
        blobs.put(b"content").await.unwrap();
        let leftovers: Vec<_> = walkdir(blobs.root())
            .into_iter()
            .filter(|path| path.to_string_lossy().contains("partial"))
            .collect();
        assert!(leftovers.is_empty(), "found {leftovers:?}");
    }

    #[tokio::test]
    async fn objects_are_sharded_two_levels_deep() {
        let (blobs, _dir) = store();
        let hash = blobs.put(b"shard me").await.unwrap();
        let expected = blobs.root().join(&hash[0..2]).join(&hash[2..4]).join(&hash);
        assert!(expected.is_file(), "expected {expected:?}");
    }

    fn walkdir(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out
    }
}
