//! Shared native catalogue discovery and attachment byte inspection.
//! Exact capture and cursors belong to the envagent registry; trace projection belongs
//! to the pure vendor mappers in `crates/trace/src/mapping/`.

use std::fs;
use std::path::{Path, PathBuf};

pub mod inventory;
pub mod timestamp;

pub use timestamp::{epoch_millis_from_iso, epoch_millis_from_json};

/// Where a vendor keeps its sessions on this machine.
///
/// Hardcoded per agent, because a native store's location is a vendor fact rather than a
/// setting: there is no configuration key for it, and inventing one would offer a lever
/// that cannot be right.
///
/// `$HOME`-relative and resolved at call time rather than at startup, so a test can
/// point `HOME` elsewhere without this crate holding a cached path from before it moved.
pub fn home_relative(segments: &[&str]) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut path = PathBuf::from(home);
    for segment in segments {
        path.push(segment);
    }
    Some(path)
}

/// One session file the store holds, before anything parses it.
///
/// The point of a candidate is that [`crate::Adapter::enumerate`] must answer "what is
/// on this machine" **without parsing every session** — a per-session metadata call on a
/// real store measured 11.5 ms, so a whole-store scan that way cost 52.9 s while one
/// listing pass over the same data was 164 ms. So a candidate carries what `stat` and
/// the path already say, and parsing is what a caller opts into per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCandidate {
    pub path: PathBuf,
    /// Size in bytes, from the directory entry.
    pub size: u64,
    /// Last modification, epoch milliseconds.
    ///
    /// This is the cheap ordering key every listing uses, and it is **not** the same
    /// fact as a session's own recorded end: a store still being written has an mtime
    /// and no end stamp. Kept separate for that reason.
    pub modified_at_ms: i64,
}

impl SessionCandidate {
    /// Read what the filesystem already knows. `None` when the entry is unreadable or
    /// is not a file, so a directory in the store cannot become a zero-byte session.
    pub fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let modified_at_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0);
        Some(Self {
            path: path.to_path_buf(),
            size: meta.len(),
            modified_at_ms,
        })
    }

    /// A cheap "has this changed" token: size and mtime.
    ///
    /// # Why not a content hash
    ///
    /// Hashing content is right for an ingest pipeline and wrong here for two reasons: it
    /// costs a full read of every file, which is the whole cost this type exists to avoid,
    /// and there is no ingest step to skip — `adopt` parses on demand and `watch` follows
    /// appends.
    ///
    /// What a caller actually needs is "is my parse of this file stale", and size+mtime
    /// answers that for the only mutation these stores perform. Two of the three
    /// append-only; `pi` truncate-rewrites, which changes the size. A same-size
    /// same-mtime edit is undetectable, and is not a shape any of the three produces.
    pub fn fingerprint(&self) -> String {
        format!("{}:{}", self.size, self.modified_at_ms)
    }
}

/// Every file under `root` matching `keep`, newest first.
///
/// Newest-first because every listing surface wants it and sorting once here is cheaper
/// than sorting per caller. Recursive, because `claude` nests one directory per project
/// and `codex` nests by date.
///
/// Unreadable directories are **skipped rather than failing the walk**: a store with one
/// permission-denied subdirectory should still list every other session, and a listing
/// that fails wholesale on it is a store that reads as empty.
pub fn candidates(root: &Path, keep: impl Fn(&Path) -> bool) -> Vec<SessionCandidate> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() => {
                    if keep(&path) {
                        if let Some(candidate) = SessionCandidate::of(&path) {
                            found.push(candidate);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    found.sort_by(|a, b| b.modified_at_ms.cmp(&a.modified_at_ms));
    found
}

// Bundle sidefiles may change without appending to the root transcript. A store-wide
// revision conservatively refreshes unrelated sessions too, but never substitutes a
// projected end timestamp for a filesystem change detector.
pub fn inventory_revision(root: &Path) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let mut paths: Vec<_> = candidates(root, |_| true)
        .into_iter()
        .map(|candidate| candidate.path)
        .collect();
    paths.sort();
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    for path in paths {
        let metadata = fs::metadata(&path).ok()?;
        path.hash(&mut digest);
        metadata.len().hash(&mut digest);
        metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
            .hash(&mut digest);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            metadata.dev().hash(&mut digest);
            metadata.ino().hash(&mut digest);
        }
    }
    Some(format!("{:016x}", digest.finish()))
}

/// A file's image type from its magic bytes, or `None` when it is not one this list knows.
///
/// **Content, not extension.** An attachment travels to a vendor inlined — `pi` wants
/// `ImageContent.data` and `claude` the same — so the type declared alongside those bytes
/// has to describe them. A `.png` that is really a JPEG would be sent with a type its
/// content contradicts, and the vendor is the one that then fails.
///
/// Twelve bytes because that is the longest prefix any of these needs: WEBP's marker is at
/// offset 8, after `RIFF` and a length. A shorter read would answer `None` for a real WEBP.
///
/// Shared rather than per-adapter: it was written out twice, byte-identical but for one
/// allocating a `Vec` the other did not, so adding a format meant two edits and knowing to
/// make both.
pub fn image_mime(path: &Path) -> Option<&'static str> {
    let mut head = [0u8; 12];
    let read = {
        use std::io::Read;
        let mut file = fs::File::open(path).ok()?;
        file.read(&mut head).ok()?
    };
    let head = &head[..read];

    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if head.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

#[cfg(test)]
mod tests {

    /// Image types come from content, and a wrong suffix does not change the answer.
    ///
    /// Untested in both places it was copied from. It decides the `mediaType` sent
    /// alongside inlined bytes, so a misdetection is a provider error a user cannot act
    /// on — and WEBP is the case a naive implementation gets wrong, since its marker sits
    /// at offset 8 rather than 0.
    #[test]
    fn image_mime_reads_magic_bytes_not_the_extension() {
        let dir = std::env::temp_dir().join(format!("sylph-mime-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            fs::write(&path, bytes).unwrap();
            path
        };

        // Deliberately mislabelled: a PNG named `.jpg` is still a PNG.
        let png = write("a.jpg", b"\x89PNG\r\n\x1a\n\x00\x00\x00\x00");
        assert_eq!(image_mime(&png), Some("image/png"));

        let jpeg = write("b.png", b"\xff\xd8\xff\xe0\x00\x10JFIF");
        assert_eq!(image_mime(&jpeg), Some("image/jpeg"));

        assert_eq!(
            image_mime(&write("c.gif", b"GIF89a\x01\x00\x01\x00\x00\x00")),
            Some("image/gif")
        );
        assert_eq!(
            image_mime(&write("d.gif", b"GIF87a\x01\x00\x01\x00\x00\x00")),
            Some("image/gif")
        );

        // The marker is at offset 8, so this needs the full twelve bytes.
        assert_eq!(
            image_mime(&write("e.webp", b"RIFF\x24\x00\x00\x00WEBPVP8 ")),
            Some("image/webp")
        );
        // A RIFF container that is not WEBP — a WAV — must not be claimed as an image.
        assert_eq!(
            image_mime(&write("f.wav", b"RIFF\x24\x00\x00\x00WAVEfmt ")),
            None
        );

        assert_eq!(
            image_mime(&write("g.txt", b"just text, not an image")),
            None
        );
        // Too short to match anything, which must answer None rather than panic.
        assert_eq!(image_mime(&write("h.png", b"\x89PN")), None);
        assert_eq!(image_mime(&dir.join("absent.png")), None);

        fs::remove_dir_all(&dir).ok();
    }
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sylph-native-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn candidates_are_found_recursively_and_ordered_newest_first() {
        let dir = temp_dir("walk");
        let nested = dir.join("project-a").join("deeper");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.join("root.jsonl"), "{}\n").unwrap();
        fs::write(nested.join("nested.jsonl"), "{}\n").unwrap();
        fs::write(dir.join("ignored.txt"), "x").unwrap();

        // mtimes on a fresh write can land in the same millisecond, so ordering is
        // asserted on the set and the filter on the extension.
        let found = candidates(&dir, |path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
        });
        let names: Vec<_> = found
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "found {names:?}");
        assert!(names.contains(&"root.jsonl".to_string()));
        assert!(names.contains(&"nested.jsonl".to_string()));
        assert!(found.iter().all(|c| c.size > 0));
    }

    /// A directory named like a session file must not become a zero-byte session.
    #[test]
    fn a_directory_is_never_a_candidate() {
        let dir = temp_dir("dir-entry");
        fs::create_dir_all(dir.join("looks-like.jsonl")).unwrap();
        assert_eq!(SessionCandidate::of(&dir.join("looks-like.jsonl")), None);
    }

    /// The staleness token has to change when the file does, or a cached parse is
    /// served for a session that has grown.
    #[test]
    fn a_fingerprint_changes_when_the_file_grows() {
        let dir = temp_dir("fingerprint");
        let path = dir.join("growing.jsonl");
        fs::write(&path, "{\"a\":1}\n").unwrap();
        let before = SessionCandidate::of(&path).unwrap().fingerprint();
        fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let after = SessionCandidate::of(&path).unwrap().fingerprint();
        assert_ne!(before, after);
    }

    #[test]
    fn native_inventory_revision_includes_bundle_sidefiles_and_removals() {
        let root = temp_dir("inventory-revision");
        fs::write(root.join("session.jsonl"), "{}\n").unwrap();
        let first = inventory_revision(&root).unwrap();
        let bundle = root.join("session/workflows");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("manifest.json"), "{}\n").unwrap();
        let with_manifest = inventory_revision(&root).unwrap();
        assert_ne!(first, with_manifest);
        fs::write(bundle.join("manifest.json"), "{\"completed\":true}\n").unwrap();
        assert_ne!(with_manifest, inventory_revision(&root).unwrap());
        fs::remove_file(bundle.join("manifest.json")).unwrap();
        assert_eq!(first, inventory_revision(&root).unwrap());
    }
}
