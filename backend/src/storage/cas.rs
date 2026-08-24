//! Content-addressed blob storage.
//!
//! Payloads over a threshold live here rather than inline, keyed by
//! `blake3(content)` and sharded two levels to keep directory sizes sane.
//!
//! Dedup is the point, not a side effect. An agent reads the same file across
//! many turns of a session and the same dependency source across many sessions;
//! addressing by hash stores that once. `docs/storage.md` argues the thresholds.
//!
//! blake3 rather than SHA-256 for throughput, and because collision resistance is
//! the only property needed: this is a content index, not a trust boundary.
//! Nothing is executed or trusted on the basis of its hash.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;

/// Payloads at or above this size are externalized.
///
/// Below a filesystem block a separate file wastes the rest of the block and
/// costs an inode, so small payloads are strictly worse outside. Above it, an
/// inline blob inflates every row scan that does not need the payload — and a
/// timeline read usually does not, since a large tool result renders collapsed
/// until expanded.
pub const EXTERNALIZE_THRESHOLD: usize = 4096;

/// Text at or above this size is compressed inline.
///
/// zstd loses to its own frame overhead on shorter strings. Above it, agent
/// output compresses 3–5×, because tool output, code, and diffs are extremely
/// redundant.
pub const COMPRESS_THRESHOLD: usize = 512;

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
    fn path_for(&self, hash: &str) -> PathBuf {
        self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash)
    }

    /// Store bytes and return their hash.
    ///
    /// Idempotent by construction: the same content yields the same path, and an
    /// existing object is left alone rather than rewritten. Skipping the rewrite
    /// is what makes storing a file an agent reads repeatedly nearly free.
    pub async fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let path = self.path_for(&hash);

        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(hash);
        }

        let parent = path
            .parent()
            .context("A blob path always has a parent directory")?;
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Could not create {:?}", parent))?;

        // Written to a temporary name and renamed, so a crash mid-write cannot
        // leave a truncated file *at the hash of its full content* — which would
        // then be trusted forever, since `put` short-circuits on existence and
        // nothing re-verifies.
        let temporary = parent.join(format!("{}.partial", hash));
        let mut file = tokio::fs::File::create(&temporary)
            .await
            .with_context(|| format!("Could not create {:?}", temporary))?;
        file.write_all(bytes).await?;
        // Durable before the rename: a rename that lands before the data would
        // survive a power loss as a valid-looking empty object.
        file.sync_all().await?;
        drop(file);

        tokio::fs::rename(&temporary, &path)
            .await
            .with_context(|| format!("Could not publish {:?}", path))?;

        Ok(hash)
    }

    /// Read an object back. `None` when it is absent.
    pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        match tokio::fs::read(self.path_for(hash)).await {
            Ok(bytes) => Ok(Some(bytes)),
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
        match tokio::fs::remove_file(self.path_for(hash)).await {
            Ok(()) => Ok(()),
            // Already gone is the desired end state, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("Could not remove a blob"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// How a block's text is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Small enough to sit in the row as-is.
    Raw(Vec<u8>),
    /// Compressed, still in the row.
    Zstd(Vec<u8>),
    /// Externalized, leaving only a hash.
    External { hash: String },
}

impl Payload {
    /// The `text_encoding` column value.
    pub fn encoding(&self) -> &'static str {
        match self {
            Self::Raw(_) => "raw",
            Self::Zstd(_) => "zstd",
            Self::External { .. } => "external",
        }
    }
}

/// Decide how to store some text, writing to `blobs` if it is large.
///
/// The thresholds are recorded per row rather than assumed globally, so a stored
/// row stays readable after they change. A design where the threshold is implicit
/// cannot be tuned without a migration.
pub async fn store_text(blobs: &BlobStore, text: &str) -> Result<Payload> {
    let bytes = text.as_bytes();

    if bytes.len() >= EXTERNALIZE_THRESHOLD {
        // Compressed before externalizing: the blob store is content-addressed by
        // what it holds, so this dedups compressed bytes. Identical text always
        // compresses identically, which is what keeps that sound.
        let compressed = zstd::encode_all(bytes, 3).context("Could not compress a payload")?;
        let hash = blobs.put(&compressed).await?;
        return Ok(Payload::External { hash });
    }

    if bytes.len() >= COMPRESS_THRESHOLD {
        let compressed = zstd::encode_all(bytes, 3).context("Could not compress a payload")?;
        // Only if it actually helped. Incompressible text — base64, random ids —
        // otherwise grows by the frame header and costs a decode on every read.
        if compressed.len() < bytes.len() {
            return Ok(Payload::Zstd(compressed));
        }
    }

    Ok(Payload::Raw(bytes.to_vec()))
}

/// Read text back from whichever form it was stored in.
pub async fn load_text(blobs: &BlobStore, payload: &Payload) -> Result<String> {
    let bytes = match payload {
        Payload::Raw(bytes) => bytes.clone(),
        Payload::Zstd(compressed) => {
            zstd::decode_all(compressed.as_slice()).context("Could not decompress a payload")?
        }
        Payload::External { hash } => {
            let compressed = blobs
                .get(hash)
                .await?
                .with_context(|| format!("Blob {} is missing", hash))?;
            zstd::decode_all(compressed.as_slice())
                .context("Could not decompress an external payload")?
        }
    };
    String::from_utf8(bytes).context("A stored payload was not valid UTF-8")
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
        assert_eq!(blobs.get(&hash).await.unwrap().as_deref(), Some(&b"hello world"[..]));
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
    async fn short_text_stays_raw_in_the_row() {
        let (blobs, _dir) = store();
        let payload = store_text(&blobs, "brief").await.unwrap();
        assert_eq!(payload.encoding(), "raw");
        assert_eq!(load_text(&blobs, &payload).await.unwrap(), "brief");
    }

    #[tokio::test]
    async fn redundant_text_over_the_threshold_is_compressed_inline() {
        let (blobs, _dir) = store();
        // Agent output is redundant in exactly this way.
        let text = "the same line of tool output\n".repeat(30);
        let payload = store_text(&blobs, &text).await.unwrap();
        assert_eq!(payload.encoding(), "zstd");
        if let Payload::Zstd(bytes) = &payload {
            assert!(bytes.len() < text.len() / 3, "should compress well");
        }
        assert_eq!(load_text(&blobs, &payload).await.unwrap(), text);
    }

    /// Incompressible text must not grow. zstd on random bytes adds a frame
    /// header and costs a decode on every read for nothing.
    #[tokio::test]
    async fn incompressible_text_is_left_raw_rather_than_inflated() {
        let (blobs, _dir) = store();
        // High-entropy and above the compress threshold but below externalizing.
        let text: String = (0..600)
            .map(|i| char::from(b'a' + ((i * 7 + i * i * 13) % 26) as u8))
            .collect();
        let payload = store_text(&blobs, &text).await.unwrap();
        if let Payload::Zstd(bytes) = &payload {
            assert!(bytes.len() < text.len(), "compression that grew the payload was kept");
        }
        assert_eq!(load_text(&blobs, &payload).await.unwrap(), text);
    }

    #[tokio::test]
    async fn large_text_is_externalized_and_reads_back_intact() {
        let (blobs, _dir) = store();
        let text = "x".repeat(EXTERNALIZE_THRESHOLD + 1);
        let payload = store_text(&blobs, &text).await.unwrap();
        assert_eq!(payload.encoding(), "external");
        assert_eq!(load_text(&blobs, &payload).await.unwrap(), text);
    }

    /// Two blocks holding the same large payload — an agent reading one file in
    /// two turns — share one object.
    #[tokio::test]
    async fn two_blocks_with_the_same_large_payload_share_one_object() {
        let (blobs, _dir) = store();
        let text = "y".repeat(EXTERNALIZE_THRESHOLD * 2);
        let first = store_text(&blobs, &text).await.unwrap();
        let second = store_text(&blobs, &text).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(walkdir(blobs.root()).len(), 1);
    }

    #[tokio::test]
    async fn multibyte_text_survives_every_storage_form() {
        let (blobs, _dir) = store();
        for text in [
            "短".to_string(),
            "中文重复内容。".repeat(80),
            "表情符号 🌱 与文字混排。".repeat(400),
        ] {
            let payload = store_text(&blobs, &text).await.unwrap();
            assert_eq!(load_text(&blobs, &payload).await.unwrap(), text);
        }
    }

    #[tokio::test]
    async fn an_external_payload_whose_blob_vanished_reports_which_one() {
        let (blobs, _dir) = store();
        let text = "z".repeat(EXTERNALIZE_THRESHOLD + 1);
        let payload = store_text(&blobs, &text).await.unwrap();
        let Payload::External { hash } = &payload else {
            panic!("expected an external payload");
        };
        blobs.remove(hash).await.unwrap();

        let error = load_text(&blobs, &payload).await.unwrap_err();
        assert!(
            format!("{error:#}").contains(hash),
            "the missing hash belongs in the message: {error:#}"
        );
    }

    /// Sharding keeps directories small; a flat layout degrades on some
    /// filesystems well before a million objects.
    #[tokio::test]
    async fn objects_are_sharded_two_levels_deep() {
        let (blobs, _dir) = store();
        let hash = blobs.put(b"shard me").await.unwrap();
        let expected = blobs
            .root()
            .join(&hash[0..2])
            .join(&hash[2..4])
            .join(&hash);
        assert!(expected.is_file(), "expected {expected:?}");
    }

    fn walkdir(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
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
