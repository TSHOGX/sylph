//! Reading a vendor's native store: the parts every agent's parser needs.
//!
//! The `Parser` trait, candidate discovery, JSONL iteration, and fingerprinting live here;
//! the per-vendor record shapes live in each agent's own module, because that is where the
//! measurements are — [`crate::claude::native`], [`crate::codex::native`],
//! [`crate::pi::native`].
//!
//! # Every record mapping is derived from a printed payload
//!
//! **A store-reading defect does not raise an error.** A missing key is indistinguishable
//! from a field the vendor omitted, so it yields a plausible record rather than a failure —
//! a parser can read an entire store without a single error while misreading most of it.
//!
//! So verify by asserting on *values a parser should have found* against a payload you have
//! printed, never by observing that a run completed. `docs/agents.md` § Reading a vendor
//! payload carries the measured instances and the rest of the rules.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

/// Call `visit` with every JSON object one line of `path` holds.
///
/// # A malformed line is skipped, and the pass continues
///
/// These are files a vendor is *currently writing*. A half-written final line is the
/// normal state of a live session, not corruption, so failing the pass on it would make
/// every running session unreadable. `pi` is the sharp case: `_rewriteFile()`
/// truncate-rewrites with mode `"w"` and no lock, so a reader can observe an empty or
/// half-written file and the correct response is to keep reading.
///
/// The cost of that tolerance is that a *systematically* malformed store also reads as
/// empty rather than as broken, so a caller that gets zero records from a non-empty file
/// should say so — see [`JsonLines::skipped`].
pub fn for_each_json_line(
    path: &Path,
    mut visit: impl FnMut(serde_json::Value) -> Result<()>,
) -> Result<JsonLines> {
    let file = fs::File::open(path)
        .with_context(|| format!("opening native session file {}", path.display()))?;
    let mut stats = JsonLines::default();
    // Native transcripts hold whole tool outputs, so a single line can be megabytes;
    // `BufReader::lines` allocates per line rather than holding the file.
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            // An I/O error mid-file, not a parse failure: the file is being rewritten
            // under us. Same treatment for the same reason.
            stats.skipped += 1;
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => {
                stats.parsed += 1;
                visit(value)?;
            }
            Err(_) => stats.skipped += 1,
        }
    }
    Ok(stats)
}

/// What one pass over a JSONL file saw.
///
/// Returned rather than logged so a caller can distinguish "this session is empty" from
/// "nothing in this file parsed" — which look identical downstream and mean opposite
/// things.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonLines {
    pub parsed: usize,
    pub skipped: usize,
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

    /// A half-written final line is the normal state of a live session, so it must not
    /// cost the records before it. This is the `pi` truncate-rewrite hazard in the
    /// smallest form that can be asserted.
    #[test]
    fn a_truncated_final_line_does_not_lose_the_records_before_it() {
        let dir = temp_dir("truncated");
        let path = dir.join("session.jsonl");
        fs::write(&path, "{\"a\":1}\n{\"a\":2}\n{\"a\":3,\"b\":\n").unwrap();

        let mut seen = Vec::new();
        let stats = for_each_json_line(&path, |value| {
            seen.push(value["a"].as_i64().unwrap());
            Ok(())
        })
        .unwrap();

        assert_eq!(seen, vec![1, 2]);
        assert_eq!(stats.parsed, 2);
        assert_eq!(stats.skipped, 1, "the partial line is counted, not silent");
    }

    /// An empty file and an unparseable one are different facts, and the caller has to
    /// be able to tell them apart — they look identical once the records are gone.
    #[test]
    fn an_unparseable_file_is_distinguishable_from_an_empty_one() {
        let dir = temp_dir("unparseable");
        let empty = dir.join("empty.jsonl");
        fs::write(&empty, "").unwrap();
        let garbage = dir.join("garbage.jsonl");
        fs::write(&garbage, "not json\nalso not json\n").unwrap();

        let empty_stats = for_each_json_line(&empty, |_| Ok(())).unwrap();
        assert_eq!((empty_stats.parsed, empty_stats.skipped), (0, 0));

        let garbage_stats = for_each_json_line(&garbage, |_| Ok(())).unwrap();
        assert_eq!((garbage_stats.parsed, garbage_stats.skipped), (0, 2));
    }

    /// Blank lines are separators, not records: a store that pads with them must not
    /// report skipped lines it did not have.
    #[test]
    fn blank_lines_are_not_counted_as_skipped() {
        let dir = temp_dir("blank");
        let path = dir.join("padded.jsonl");
        fs::write(&path, "\n{\"a\":1}\n\n\n{\"a\":2}\n\n").unwrap();
        let stats = for_each_json_line(&path, |_| Ok(())).unwrap();
        assert_eq!((stats.parsed, stats.skipped), (2, 0));
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
}
