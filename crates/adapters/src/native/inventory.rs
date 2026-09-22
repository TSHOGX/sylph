//! Vendor file discovery and cheap catalogue metadata. Capture reads every lexical byte
//! through the envagent's shared source registry; this module never projects a trace.

use super::{
    candidates, epoch_millis_from_iso, home_relative, inventory_revision, SessionCandidate,
};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use sylph_protocol::{
    normalize_workdir, Fidelity, NativeId, NativeSource, NativeSourceFormat, NativeSummary,
    SessionEdgeRelation, StoreFilter,
};

fn roots(vendor: &str) -> Vec<PathBuf> {
    let parts: &[&[&str]] = match vendor {
        "claude" => &[&[".claude", "projects"]],
        "codex" => &[&[".codex", "sessions"], &[".codex", "archived_sessions"]],
        "pi" => &[&[".pi", "agent", "sessions"]],
        _ => &[],
    };
    parts
        .iter()
        .filter_map(|parts| home_relative(parts))
        .collect()
}
fn files(vendor: &str) -> Vec<SessionCandidate> {
    let mut files = Vec::new();
    for root in roots(vendor) {
        files.extend(candidates(&root, |path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                && (vendor != "claude"
                    || path.parent().and_then(Path::parent) == Some(root.as_path()))
        }));
    }
    files
}
fn read_window(path: &Path, offset: u64, length: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}
fn records(bytes: &[u8], skip_first: bool) -> impl DoubleEndedIterator<Item = Value> + '_ {
    let bytes = if skip_first {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |index| &bytes[index + 1..])
    } else {
        bytes
    };
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| line.ends_with(b"\n"))
        .filter_map(|line| serde_json::from_slice(line).ok())
}
fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
#[derive(Clone, PartialEq, Eq)]
struct MetadataVersion {
    length: u64,
    modified: Option<std::time::SystemTime>,
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanos: i64,
}
fn metadata_version(path: &Path) -> Option<MetadataVersion> {
    use std::os::unix::fs::MetadataExt;
    let value = std::fs::metadata(path).ok()?;
    Some(MetadataVersion {
        length: value.len(),
        modified: value.modified().ok(),
        device: value.dev(),
        inode: value.ino(),
        changed_seconds: value.ctime(),
        changed_nanos: value.ctime_nsec(),
    })
}
struct CachedSummary {
    version: MetadataVersion,
    summary: Option<NativeSummary>,
    used: u64,
}
#[derive(Default)]
struct CatalogueCache {
    items: BTreeMap<(String, PathBuf), CachedSummary>,
    clock: u64,
}
fn summary(vendor: &str, candidate: &SessionCandidate) -> Option<NativeSummary> {
    // A native follower revisits the inventory, but unchanged catalogue headers need not
    // be parsed on every pass. This is bounded advisory metadata only; lexical capture
    // verifies its own complete source prefix and never trusts a cache fingerprint.
    static CACHE: std::sync::OnceLock<std::sync::Mutex<CatalogueCache>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (vendor.to_owned(), candidate.path.clone());
    let version = metadata_version(&candidate.path)?;
    {
        let mut cache = cache.lock().unwrap();
        cache.clock = cache.clock.saturating_add(1);
        let clock = cache.clock;
        if let Some(entry) = cache.items.get_mut(&key) {
            if entry.version == version {
                entry.used = clock;
                return entry.summary.clone();
            }
        }
    }
    let summary = read_summary(vendor, candidate);
    if metadata_version(&candidate.path).as_ref() == Some(&version) {
        let mut cache = cache.lock().unwrap();
        let clock = cache.clock;
        if cache.items.len() >= 10000 && !cache.items.contains_key(&key) {
            if let Some(oldest) = cache
                .items
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            {
                cache.items.remove(&oldest);
            }
        }
        cache.items.insert(
            key,
            CachedSummary {
                version,
                summary: summary.clone(),
                used: clock,
            },
        );
    }
    summary
}

fn read_summary(vendor: &str, candidate: &SessionCandidate) -> Option<NativeSummary> {
    let head = read_window(&candidate.path, 0, 1024 * 1024)?;
    let values: Vec<_> = records(&head, false).collect();
    let (id, metadata) = match vendor {
        "codex" => {
            let first = values.first()?;
            if first.get("type")?.as_str()? != "session_meta" {
                return None;
            }
            let payload = first.get("payload")?;
            (text(payload, "id")?, payload)
        }
        "pi" => {
            let first = values.first()?;
            if first.get("type")?.as_str()? != "session" {
                return None;
            }
            (text(first, "id")?, first)
        }
        "claude" => {
            let first = values
                .iter()
                .find(|value| text(value, "sessionId").is_some())?;
            (text(first, "sessionId")?, first)
        }
        _ => return None,
    };
    let workdir = values
        .iter()
        .find_map(|value| text(value, "cwd"))
        .or_else(|| text(metadata, "cwd"));
    let title =
        values
            .iter()
            .rev()
            .find_map(|value| match value.get("type").and_then(Value::as_str) {
                Some("custom-title") => text(value, "customTitle"),
                Some("ai-title") => text(value, "aiTitle"),
                Some("agent-name") => text(value, "agentName"),
                Some("session_info") => text(value, "name"),
                _ => None,
            });
    let started = values.iter().find_map(|value| {
        value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_millis_from_iso)
    });
    let tail_offset = candidate.size.saturating_sub(64 * 1024);
    let tail = read_window(&candidate.path, tail_offset, 64 * 1024)?;
    let ended = records(&tail, tail_offset > 0)
        .filter_map(|value| {
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso)
        })
        .next_back();
    let parent = match vendor {
        "codex" => text(metadata, "parent_thread_id")
            .or_else(|| {
                metadata
                    .pointer("/source/subagent/thread_spawn/parent_thread_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .map(|id| (NativeId::new(id), SessionEdgeRelation::Delegation))
            .or_else(|| {
                text(metadata, "forked_from_id")
                    .map(|id| (NativeId::new(id), SessionEdgeRelation::Fork))
            }),
        "claude" => values.iter().find_map(|value| {
            value
                .pointer("/forkedFrom/sessionId")
                .and_then(Value::as_str)
                .map(|id| (NativeId::new(id), SessionEdgeRelation::Fork))
        }),
        "pi" => None,
        _ => None,
    };
    Some(NativeSummary {
        native_id: NativeId::new(id),
        workdir,
        title,
        started_at_ms: started,
        ended_at_ms: ended,

        source_revision: None,
        parent_native_id: parent.as_ref().map(|(id, _)| id.clone()),
        parent_relation: parent.map(|(_, relation)| relation),
        fidelity: if vendor == "pi" {
            Fidelity::Messages
        } else {
            Fidelity::Blocks
        },
    })
}

pub fn enumerate(vendor: &str, filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
    let revision = roots(vendor)
        .iter()
        .filter_map(|root| inventory_revision(root))
        .collect::<Vec<_>>()
        .join(":");
    let mut summaries = BTreeMap::<NativeId, NativeSummary>::new();
    for candidate in files(vendor) {
        let Some(mut value) = summary(vendor, &candidate) else {
            continue;
        };
        if filter.workdir.as_ref().is_some_and(|wanted| {
            value
                .workdir
                .as_ref()
                .is_none_or(|actual| normalize_workdir(actual) != normalize_workdir(wanted))
        }) {
            continue;
        }
        if filter
            .since_ms
            .is_some_and(|since| value.ended_at_ms.unwrap_or(candidate.modified_at_ms) < since)
        {
            continue;
        }
        value.source_revision = Some(revision.clone());
        summaries.entry(value.native_id.clone()).or_insert(value);
    }
    let mut summaries: Vec<_> = summaries.into_values().collect();
    summaries
        .sort_by_key(|summary| std::cmp::Reverse(summary.ended_at_ms.or(summary.started_at_ms)));
    summaries.truncate(filter.limit.unwrap_or(10000));
    Ok(summaries)
}
pub fn lookup(vendor: &str, id: &NativeId) -> Result<Option<NativeSummary>> {
    // Exact identity lookup cannot inherit the listing page limit: an old native
    // session is still resumable after newer sessions fill the catalogue window.
    Ok(files(vendor)
        .into_iter()
        .filter_map(|candidate| summary(vendor, &candidate))
        .find(|summary| &summary.native_id == id))
}

pub fn sources(vendor: &str, id: &NativeId) -> Result<Vec<NativeSource>> {
    sources_from(vendor, id, files(vendor))
}

fn sources_from(vendor: &str, id: &NativeId, all: Vec<SessionCandidate>) -> Result<Vec<NativeSource>> {
    let metadata: Vec<_> = all
        .iter()
        .filter_map(|file| summary(vendor, file).map(|summary| (file.path.clone(), summary)))
        .collect();
    let mut selected = BTreeSet::from([id.clone()]);
    if vendor == "codex" {
        loop {
            let previous = selected.len();
            for (_, summary) in &metadata {
                if summary.parent_relation == Some(SessionEdgeRelation::Delegation)
                    && summary
                        .parent_native_id
                        .as_ref()
                        .is_some_and(|parent| selected.contains(parent))
                {
                    selected.insert(summary.native_id.clone());
                }
            }
            if previous == selected.len() {
                break;
            }
        }
    }
    let mut paths: BTreeMap<PathBuf, Option<String>> = metadata
        .iter()
        .filter(|(_, summary)| selected.contains(&summary.native_id))
        .map(|(path, _)| (path.clone(), None))
        .collect();
    if vendor == "codex" {
        include_history_bases(&mut paths, &all);
    }
    if paths.is_empty() && vendor != "codex" {
        return Err(anyhow!(
            "native {vendor} source for {id} is not currently available"
        ));
    }
    if vendor == "claude" {
        for root in paths.clone().into_keys() {
            let interior = root.with_extension("");
            for candidate in candidates(&interior, |_| true) {
                let companion = if candidate.path.extension().is_some_and(|ext| ext == "json")
                    && candidate.path.to_string_lossy().ends_with(".meta.json")
                {
                    let path = PathBuf::from(
                        candidate
                            .path
                            .to_string_lossy()
                            .trim_end_matches(".meta.json")
                            .to_owned()
                            + ".jsonl",
                    );
                    Some(path.to_string_lossy().into_owned())
                } else {
                    // Workflow manifests and journals belong to the containing
                    // transcript bundle. The companion's retained native record
                    // proves the root identity; directory names never mint it.
                    let relative = candidate.path.strip_prefix(&interior).ok();
                    let workflow_manifest = relative.is_some_and(|path| {
                        path.parent() == Some(Path::new("workflows"))
                            && path.extension().is_some_and(|ext| ext == "json")
                    });
                    let workflow_journal = relative.is_some_and(|path| {
                        path.starts_with("subagents/workflows")
                            && path.file_name().is_some_and(|name| name == "journal.jsonl")
                    });
                    (workflow_manifest || workflow_journal)
                        .then(|| root.to_string_lossy().into_owned())
                };
                paths.insert(candidate.path, companion);
            }
        }
    }
    // A fork path locates evidence but never supplies the referenced native id.
    if vendor == "pi" {
        for path in paths.clone().into_keys() {
            if let Some(bytes) = read_window(&path, 0, 1024 * 1024) {
                if let Some(header) = records(&bytes, false).next() {
                    if let Some(parent) =
                        text(&header, "parentSession").or_else(|| text(&header, "parent_session"))
                    {
                        let parent = PathBuf::from(parent);
                        if parent.is_file() {
                            paths.insert(parent, None);
                        }
                    }
                }
            }
        }
    }
    let mut result: Vec<_> = paths
        .into_iter()
        .map(|(path, companion_locator)| {
            let proof = native_owner(
                vendor,
                companion_locator.as_deref().map(Path::new).unwrap_or(&path),
            );
            NativeSource {
                format: match path.extension().and_then(|ext| ext.to_str()) {
                    Some("jsonl") => NativeSourceFormat::Jsonl,
                    Some("json") => NativeSourceFormat::Snapshot,
                    _ => NativeSourceFormat::Bytes,
                },
                namespace: format!("{vendor}.native"),
                locator: path.to_string_lossy().into_owned(),
                owner: proof.as_ref().map(|(owner, _)| owner.clone()),
                identity_record_ordinal: if companion_locator.is_none() {
                    proof.map(|(_, ordinal)| ordinal)
                } else {
                    None
                },
                companion_locator,
            }
        })
        .collect();
    // The database schema version is a vendor contract, not an mtime selection. This
    // installed/pinned Codex schema records the selected rollout in threads.rollout_path.
    if vendor == "codex" {
        if let Some(database) = home_relative(&[".codex", "state_5.sqlite"]) {
            result.push(NativeSource {
                locator: database.to_string_lossy().into_owned(),
                namespace: format!("codex.sqlite.threads:{}", id.as_str()),
                format: NativeSourceFormat::CodexThreadMetadata,
                companion_locator: None,
                owner: Some(sylph_protocol::NativeOwner::Session {
                    native_id: id.clone(),
                }),
                identity_record_ordinal: None,
            });
        }
    }
    if vendor=="codex" {
        if let Some(database)=home_relative(&[".codex","goals_1.sqlite"]) {
            result.push(NativeSource {locator:database.to_string_lossy().into_owned(),namespace:format!("codex.sqlite.goals:{}",id.as_str()),format:NativeSourceFormat::CodexGoalMetadata,companion_locator:None,owner:None,identity_record_ordinal:None});
        }
    }
    if vendor=="codex" {
        if let Some(root)=std::env::var_os("CODEX_ROLLOUT_TRACE_ROOT").map(PathBuf::from) {
            for candidate in candidates(&root,|path|path.file_name().is_some_and(|name|name=="trace.jsonl")) {
                let Some(bytes)=read_window(&candidate.path,0,1024*1024) else {continue;};
                let contains=records(&bytes,false).any(|record|record.get("thread_id").and_then(Value::as_str)==Some(id.as_str()) || record.pointer("/payload/root_thread_id").and_then(Value::as_str)==Some(id.as_str()));
                if contains {result.push(NativeSource {locator:candidate.path.to_string_lossy().into_owned(),namespace:"codex.runtime-trace".into(),format:NativeSourceFormat::Jsonl,companion_locator:None,owner:None,identity_record_ordinal:None});}
            }
        }
    }
    Ok(result)
}

fn include_history_bases(paths: &mut BTreeMap<PathBuf, Option<String>>, all: &[SessionCandidate]) {
    // A physical rollout id locates a file under the vendor's filename contract.
    // It never substitutes for the logical id in that file's retained session_meta.
    let mut examined = BTreeSet::new();
    loop {
        let next: Vec<_> = paths
            .keys()
            .filter(|path| !examined.contains(*path))
            .cloned()
            .collect();
        if next.is_empty() {
            break;
        }
        for path in next {
            examined.insert(path.clone());
            let Some(bytes) = read_window(&path, 0, 1024 * 1024) else {
                continue;
            };
            for record in records(&bytes, false) {
                if record.get("type").and_then(Value::as_str) != Some("session_meta") {
                    continue;
                }
                let Some(physical) = record
                    .pointer("/payload/history_base/thread_id")
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let suffix = format!("-{physical}.jsonl");
                for candidate in all.iter().filter(|candidate| {
                    candidate
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(&suffix))
                }) {
                    paths.entry(candidate.path.clone()).or_insert(None);
                }
            }
        }
    }
}

fn native_owner(vendor: &str, path: &Path) -> Option<(sylph_protocol::NativeOwner, u64)> {
    let bytes = read_window(path, 0, 1024 * 1024)?;
    for (ordinal, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if !line.ends_with(b"\n") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let owner = match vendor {
            "codex" if record.get("type").and_then(Value::as_str) == Some("session_meta") => {
                text(record.get("payload")?, "id").map(|id| sylph_protocol::NativeOwner::Session {
                    native_id: NativeId::new(id),
                })
            }
            "pi" if record.get("type").and_then(Value::as_str) == Some("session") => {
                text(&record, "id").map(|id| sylph_protocol::NativeOwner::Session {
                    native_id: NativeId::new(id),
                })
            }
            "claude" => text(&record, "sessionId").map(|root| match text(&record, "agentId") {
                Some(child) => sylph_protocol::NativeOwner::Embedded {
                    anchor_native_id: NativeId::new(root),
                    namespace: "claude.agent".into(),
                    native_child_id: child,
                },
                None => sylph_protocol::NativeOwner::Session {
                    native_id: NativeId::new(root),
                },
            }),
            _ => None,
        };
        if let Some(owner) = owner {
            return Some((owner, ordinal as u64));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylph_protocol::NativeOwner;

    #[test]
    fn workflow_bundle_sources_cite_the_observed_root_transcript_without_filename_identity() {
        let directory = std::env::temp_dir().join(format!("sylph-workflow-inventory-{}", uuid::Uuid::new_v4()));
        let root = directory.join("misleading-root.jsonl");
        let bundle = root.with_extension("");
        let manifest = bundle.join("workflows/wf-one.json");
        let journal = bundle.join("subagents/workflows/wf-one/journal.jsonl");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&root, b"{\"type\":\"user\",\"sessionId\":\"observed-root\",\"uuid\":\"request\",\"message\":{\"role\":\"user\",\"content\":\"fixture\"}}\n").unwrap();
        std::fs::write(&manifest, b"{\"runId\":\"wf-one\",\"status\":\"failed\"}").unwrap();
        std::fs::write(&journal, b"{\"type\":\"started\",\"key\":\"invocation\",\"agentId\":\"actual-child\"}\n").unwrap();
        let sources = sources_from("claude", &NativeId::new("observed-root"), vec![SessionCandidate::of(&root).unwrap()]).unwrap();
        assert_eq!(sources.len(), 3);
        for (path, format) in [(&manifest, NativeSourceFormat::Snapshot), (&journal, NativeSourceFormat::Jsonl)] {
            let source = sources.iter().find(|source| source.locator == path.to_string_lossy()).unwrap();
            assert_eq!(source.format, format);
            assert_eq!(source.owner, Some(NativeOwner::Session { native_id: NativeId::new("observed-root") }));
            assert_eq!(source.companion_locator.as_deref(), root.to_str());
            assert_eq!(source.identity_record_ordinal, None);
        }
        assert!(sources_from("claude", &NativeId::new("misleading-root"), vec![SessionCandidate::of(&root).unwrap()]).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn physical_history_bases_are_followed_without_selecting_one_duplicate_locator() {
        let directory =
            std::env::temp_dir().join(format!("sylph-history-bases-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let write = |name: &str, id: &str, base: Option<&str>| {
            let path = directory.join(name);
            let mut header = serde_json::json!({"type":"session_meta","payload":{"id":id}});
            if let Some(base) = base {
                header["payload"]["history_base"] = serde_json::json!({"thread_id":base,"end_ordinal_exclusive":3,"end_byte_offset_exclusive":99});
            }
            let mut bytes = serde_json::to_vec(&header).unwrap();
            bytes.push(b'\n');
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let root = write("rollout-current.jsonl", "logical", Some("physical-1"));
        let base = write(
            "rollout-time-physical-1.jsonl",
            "ancestor",
            Some("physical-0"),
        );
        let copied = write(
            "rollout-copy-physical-1.jsonl",
            "ancestor",
            Some("physical-0"),
        );
        let oldest = write("rollout-time-physical-0.jsonl", "oldest", None);
        let all = [&root, &base, &copied, &oldest]
            .into_iter()
            .map(|path| SessionCandidate::of(path).unwrap())
            .collect::<Vec<_>>();
        let mut selected = BTreeMap::from([(root, None)]);
        include_history_bases(&mut selected, &all);
        assert_eq!(selected.len(), 4);
        assert!(selected.contains_key(&base));
        assert!(selected.contains_key(&copied));
        assert!(selected.contains_key(&oldest));
        assert_eq!(
            native_owner("codex", &base).unwrap().0,
            NativeOwner::Session {
                native_id: NativeId::new("ancestor")
            }
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn identity_metadata_uses_lexical_ordinal_and_never_the_filename() {
        let directory = std::env::temp_dir().join(format!("sylph-native-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("invented-id.jsonl");
        std::fs::write(&file, b"\r\nnot-json\n{\"type\":\"queue-operation\"}\n{\"sessionId\":\"root\",\"agentId\":\"child\",\"type\":\"user\"}\n").unwrap();
        assert_eq!(
            native_owner("claude", &file),
            Some((
                NativeOwner::Embedded {
                    anchor_native_id: NativeId::new("root"),
                    namespace: "claude.agent".into(),
                    native_child_id: "child".into()
                },
                3
            ))
        );
        std::fs::write(&file, b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"actual\",\"cwd\":\"/recorded\",\"source\":{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"parent\"}}}}}\n").unwrap();
        let found = summary("codex", &SessionCandidate::of(&file).unwrap()).unwrap();
        assert_eq!(found.native_id, NativeId::new("actual"));
        assert_eq!(found.parent_native_id, Some(NativeId::new("parent")));
        assert_eq!(found.parent_relation, Some(SessionEdgeRelation::Delegation));
        assert_eq!(found.workdir.as_deref(), Some("/recorded"));
        std::fs::write(
            &file,
            b"{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/recorded\"}}\n",
        )
        .unwrap();
        assert!(native_owner("codex", &file).is_none());
        assert!(summary("codex", &SessionCandidate::of(&file).unwrap()).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

/// Exact source selectors disambiguate multiple results in one native record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarReference {
    pub locator: String,
    pub parent_slot: String,
    pub relation: String,
}

/// Routing metadata is extracted only after lexical retention. File contents are never
/// executed, and an output path cannot supply agent or operation identity.
pub fn sidecar_references(raw: &[u8]) -> Vec<SidecarReference> {
    fn join(base: &str, key: &str) -> String {
        format!("{base}/{}", key.replace('~', "~0").replace('/', "~1"))
    }
    fn visit(value: &Value, slot: &str, found: &mut BTreeSet<(String, String, String)>) {
        if let Some(object) = value.as_object() {
            if value.get("raw_payload_id").and_then(Value::as_str).is_some() {
                if let (Some(path),Some(kind))=(value.get("path").and_then(Value::as_str),value.pointer("/kind/type").and_then(Value::as_str)) {
                    found.insert((path.into(),slot.into(),format!("codex.raw.{kind}")));
                }
            }
            if let Some(result) = object.get("toolUseResult") {
                let candidates: Vec<_> = value
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, part)| {
                        part.get("type").and_then(Value::as_str) == Some("tool_result")
                    })
                    .collect();
                let parent = if let [(index, _)] = candidates.as_slice() {
                    format!("{slot}/message/content/{index}")
                } else {
                    join(slot, "toolUseResult")
                };
                paths(result, &parent, found);
            }
            if value.get("role").and_then(Value::as_str) == Some("toolResult") {
                if let Some(details) = value.get("details") {
                    paths(details, slot, found);
                }
            }
            if value.get("type").and_then(Value::as_str) == Some("tool_execution_end") {
                if let Some(result) = value.pointer("/result/details") {
                    paths(result, &join(slot, "result"), found);
                }
            }
            for (key, child) in object {
                visit(child, &join(slot, key), found);
            }
        } else if let Some(items) = value.as_array() {
            for (index, child) in items.iter().enumerate() {
                visit(child, &join(slot, &index.to_string()), found);
            }
        }
    }
    fn paths(value: &Value, slot: &str, found: &mut BTreeSet<(String, String, String)>) {
        for key in ["persistedOutputPath", "outputFile", "fullOutputPath"] {
            if let Some(path) = value
                .get(key)
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
            {
                found.insert((path.into(), slot.into(),"tool_output".into()));
            }
        }
    }
    let mut found = BTreeSet::new();
    if let Ok(value) = serde_json::from_slice::<Value>(raw) {
        visit(&value, "", &mut found);
    }
    found
        .into_iter()
        .map(|(locator, parent_slot,relation)| SidecarReference {locator,parent_slot,relation})
        .collect()
}
