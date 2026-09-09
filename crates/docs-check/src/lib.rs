//! Checks that every pointer this repo's prose makes still resolves.
//!
//! Three kinds of pointer rot, all of which this repo has actually shipped, and
//! none of which any other gate can see. A rotted pointer is the defect class
//! `AGENTS.md` § Where Design Rules Live is about: it produces a plausible
//! reading rather than an error, so a reader follows it, finds nothing, and
//! cannot tell whether the document is wrong or they are.
//!
//! Three forms are checked. Each has rotted here at least once:
//!
//! - **A section pointer** — a document, `§`, and a heading. Rots on a rename, and
//!   the specs rewrite renamed four sections out from under live pointers.
//! - **A backticked repo path.** Rots when a file moves or is deleted. The deleted
//!   `adapter/link.rs` under `crates/server/src/` was still named by three crate
//!   comments, one of which described its behaviour in the present tense.
//! - **A markdown link,** with its `#anchor`.
//!
//! `titles_agree` owns how loosely a heading is matched, and why.
//!
//! Fixtures are excluded. A vendor's captured payload legitimately contains paths
//! from the machine it was recorded on, and rewriting one to satisfy a lint would
//! corrupt the evidence it exists to be.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One pointer that does not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Repo-relative, so the message is pasteable as a file:line.
    pub file: String,
    pub line: usize,
    pub pointer: String,
    pub problem: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {} — {}",
            self.file, self.line, self.problem, self.pointer
        )
    }
}

/// The workspace root, derived from this crate's own location.
pub fn repo_root() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while !path.join("docs/README.md").is_file() {
        let Some(parent) = path.parent() else { break };
        path = parent.to_path_buf();
    }
    path
}

/// Directories holding nothing this lint governs.
///
/// `fixtures` is here for a reason of substance rather than speed: see the module
/// note. The rest are build output and vendored trees.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "fixtures",
    ".e2e-home",
    "test-results",
    ".next",
    "playwright-report",
];

/// Extensions whose prose is checked.
const SCANNED: &[&str] = &["rs", "ts", "tsx", "md", "toml", "sql", "mjs"];

/// Every file this lint reads, repo-relative and sorted.
pub fn sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

/// Every unresolved pointer in the repo, in file order.
pub fn check(root: &Path) -> Vec<Finding> {
    let mut cache: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
    let mut findings = Vec::new();

    for relative in sources(root) {
        let display = relative.display().to_string();
        let Ok(body) = fs::read_to_string(root.join(&relative)) else {
            continue;
        };
        let own_headings = if display.ends_with(".md") {
            headings(&body)
        } else {
            Vec::new()
        };

        let lines: Vec<&str> = body.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let line_number = index + 1;
            // A wrapped pointer leaves the document on the line above the `§`, so the
            // previous line is joined in for extraction only. Both spellings are
            // common in this repo and neither is worth rewriting to satisfy a lint.
            let previous = index.checked_sub(1).map(|i| lines[i]).unwrap_or_default();
            let mut push = |pointer: String, problem: String| {
                findings.push(Finding {
                    file: display.clone(),
                    line: line_number,
                    pointer,
                    problem,
                })
            };

            check_section_pointers(
                root,
                &display,
                line,
                previous,
                &own_headings,
                &mut cache,
                &mut push,
            );
            check_code_paths(root, &display, line, &mut push);
            check_markdown_links(root, &display, line, &mut cache, &mut push);
        }
    }

    findings
}

/// Where a cited path actually is, repo-relative.
///
/// A citation is written from wherever the reader is standing, and this repo uses
/// three spellings on purpose: repo-relative from a doc (`docs/api/files.md`),
/// relative to the citing file's own directory (a README naming a sibling
/// directory), and relative to an ancestor of it (a module under `crates/server/src/`
/// naming a migration by bare filename). Trying the root first and
/// then walking up from the citing file resolves all three without the lint having
/// to know which convention a file follows.
fn resolve_document(root: &Path, from: &str, cited: &str) -> Option<String> {
    if root.join(cited).exists() {
        return Some(cited.to_string());
    }
    let mut base = Path::new(from).parent();
    while let Some(directory) = base {
        let candidate = normalize_relative(&directory.join(cited));
        if root.join(&candidate).exists() {
            return Some(candidate.display().to_string());
        }
        base = directory.parent();
    }
    None
}

/// Headings of `document`, or `None` when it does not exist.
fn document_headings<'a>(
    root: &Path,
    document: &str,
    cache: &'a mut BTreeMap<String, Option<Vec<String>>>,
) -> &'a Option<Vec<String>> {
    cache.entry(document.to_string()).or_insert_with(|| {
        fs::read_to_string(root.join(document))
            .ok()
            .map(|body| headings(&body))
    })
}

/// The document a `§` pointer names, and the section title after it.
///
/// `None` for the document means the pointer is same-file (`§ Promotion`), which
/// is legal in a markdown file and meaningless in code.
pub fn split_section_pointer(line: &str, at: usize) -> (Option<String>, String) {
    let (before, after) = line.split_at(at);
    let title = after
        .trim_start_matches('§')
        .split(['`', '(', ')', '|', ';', '"'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();

    // The document is the last `.md` token before the `§`. Taken from the end
    // because a sentence may name several files before pointing into one.
    let document = before
        .trim_end()
        .trim_end_matches('`')
        .split_whitespace()
        .next_back()
        // A pointer often follows a markdown link — "[ADR-0006](../adr/0006-….md)
        // § Three capabilities" — so the token is the link's target, not the whole
        // link. Taking what follows the last `(` recovers it.
        .map(|token| {
            token
                .rsplit('(')
                .next()
                .unwrap_or(token)
                .trim_matches(['`', '(', ')', ',', '*', '[', ']'])
                .to_string()
        })
        .filter(|token| token.ends_with(".md"));

    (document, title)
}

/// Words of a heading or title, lowercased and stripped of edge punctuation.
///
/// A leading enumerator is dropped. An ADR numbers its decisions — "### 2. Three
/// capabilities, split where the risk changes" — and every pointer at one cites it
/// by name, which is the right way round: a reader finds the section by what it
/// says, and renumbering an ADR must not invalidate a citation.
fn words(text: &str) -> Vec<String> {
    let text = text
        .trim_start()
        .split_once(". ")
        .filter(|(head, _)| head.chars().all(|c| c.is_ascii_digit()) && !head.is_empty())
        .map(|(_, tail)| tail)
        .unwrap_or(text);
    normalize(text)
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `title` names `heading`.
///
/// # Why this is a word-sequence rule and not string equality
///
/// A pointer is written inside a sentence — "the session protocol's § Requests
/// governs" — so the title cannot be delimited from the prose that follows it, and
/// this repo's pointers legitimately name a *shortened* heading: `§ Promotion` for
/// "Promotion: a trajectory is defined by use", `§ Outcomes` for "Outcomes, and
/// what a turn may close". Requiring the full heading would fail hundreds of
/// correct pointers, and demanding a delimiter would make the prose worse.
///
/// So a match needs the two to share a leading word sequence, long enough that a
/// rename cannot slip through. Two words is that threshold, plus the cases where
/// one word is genuinely the whole name: the shared run exhausts the title, the
/// heading, or the heading's short form (its text before the first `:` or `,`,
/// which is how every shortened pointer here is spelled — the delimiter set also
/// covers an em-dash and a parenthesis, since "E2E — Playwright (`npm run e2e`)"
/// is cited as `§ E2E`).
///
/// What it deliberately cannot catch: a pointer naming a real heading followed by
/// wrong prose. Nothing mechanical can, and no reader is misled by it. What it
/// does catch is the whole rot class — `§ What changed` shares one word with
/// "What the adapter owns", exhausts neither, and is reported.
fn titles_agree(title: &str, heading: &str) -> bool {
    let title = words(title);
    if title.is_empty() {
        return true;
    }
    let full = words(heading);
    let shared = full
        .iter()
        .zip(&title)
        .take_while(|(a, b)| a == b)
        .count();
    if shared == 0 {
        return false;
    }
    // The heading's short form: what a pointer names when the heading has a
    // colon-or-comma tail explaining it.
    let short = heading
        .split([':', ',', '—', '('])
        .next()
        .map(|part| words(part).len())
        .unwrap_or(full.len());

    shared >= 2 || shared == title.len() || shared == full.len() || shared >= short
}

/// Compare form for a heading or a pointer title.
///
/// Case, backticks, and trailing punctuation all vary between a heading and the
/// sentence that points at it, and none of them carries meaning here.
pub fn normalize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c == '`' || c == '*' { ' ' } else { c })
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches([',', '.', ';', ':', '|', '—', '-'])
        .trim()
        .to_lowercase()
}

/// GitHub's anchor form for a heading, for checking `#fragment` links.
fn anchor_of(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Every ATX heading in a markdown file, in source order.
///
/// Fenced code is skipped: a shell transcript or a Rust snippet containing a `#`
/// comment is not a heading, and treating it as one would let a pointer resolve
/// against a comment.
pub fn headings(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !trimmed.starts_with('#') {
            continue;
        }
        let title = trimmed.trim_start_matches('#').trim();
        if !title.is_empty() {
            found.push(title.to_string());
        }
    }
    found
}

#[allow(clippy::too_many_arguments)]
fn check_section_pointers(
    root: &Path,
    from: &str,
    line: &str,
    previous: &str,
    own_headings: &[String],
    cache: &mut BTreeMap<String, Option<Vec<String>>>,
    push: &mut impl FnMut(String, String),
) {
    for (at, _) in line.match_indices('§') {
        let (mut document, title) = split_section_pointer(line, at);
        // Wrapped: the document is the tail of the line above. Only when nothing
        // but comment markers precedes the `§` on this line, because a line that
        // *has* text before it and no `.md` is a same-file pointer — and joining
        // there would adopt whichever document the previous sentence happened to
        // name last, which is a wrong answer rather than a missing one.
        let leader = line[..at].trim().trim_start_matches(['/', '!', '*', '#', '>']);
        if document.is_none() && leader.trim().is_empty() {
            let joined = format!("{previous} {line}");
            document = split_section_pointer(&joined, previous.len() + 1 + at).0;
        }
        let pointer = match &document {
            Some(document) => format!("{document} § {title}"),
            None => format!("§ {title}"),
        };

        // A bare section number is a pointer into somebody else's specification
        // (`§ 5.2.2.6` names an RFC), not into a document here.
        if title.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }

        let (target, available) = match document {
            Some(document) => {
                let Some(resolved) = resolve_document(root, from, &document) else {
                    // A pointer into somebody else's documentation — a vendor
                    // package's own `docs/` — names a file this repo does not have
                    // and must not be asked to produce. Only a document that
                    // resolves, or one spelled as ours, is checked.
                    if is_ours(&document) {
                        push(pointer, "no such document".into());
                    }
                    continue;
                };
                let Some(found) = document_headings(root, &resolved, cache).clone() else {
                    push(pointer, "no such document".into());
                    continue;
                };
                (resolved, found)
            }
            // A same-file pointer in code names nothing a reader can follow, but
            // it is also how a doc comment refers to its own module's section, so
            // it is only checked inside markdown.
            None if from.ends_with(".md") => (from.to_string(), own_headings.to_vec()),
            None => continue,
        };

        if title.is_empty() || available.iter().any(|h| titles_agree(&title, h)) {
            continue;
        }
        push(pointer, format!("no such section in {target}"));
    }
}

/// A backticked path that looks like this repo's own and does not exist.
///
/// Deliberately narrow. Only a token containing `/`, starting at a real top-level
/// directory, and ending in a source extension is checked, because prose is full
/// of paths belonging to vendors, other machines, and hypotheticals — and a lint
/// that flags those gets turned off.
/// Whether a cited path is spelled as one of this repo's own.
///
/// Deliberately narrow. Prose here is full of paths belonging to vendors, other
/// machines, and hypotheticals, and a lint that flags those is a lint somebody
/// turns off.
fn is_ours(candidate: &str) -> bool {
    const OWNED: &[&str] = &[
        "crates/server/",
        "crates/",
        "frontend/",
        "docs/",
        "scripts/",
        "migrations/",
    ];
    candidate.contains('/') && OWNED.iter().any(|dir| candidate.starts_with(dir))
}

fn check_code_paths(root: &Path, from: &str, line: &str, push: &mut impl FnMut(String, String)) {
    for token in line.split('`').skip(1).step_by(2) {
        let candidate = token.trim();
        if !is_ours(candidate) {
            continue;
        }
        // A path with a line suffix, a glob, or a trailing marker is a reference
        // to a region rather than a file; the file part is what gets checked.
        let path = candidate.split([':', ' ', '#']).next().unwrap_or(candidate);
        if path.contains('*') || path.contains('{') || path.contains("...") {
            continue;
        }
        let Some(extension) = Path::new(path).extension().and_then(|e| e.to_str()) else {
            // A directory reference: `crates/adapters/` is a real pointer too.
            if path.ends_with('/') && resolve_document(root, from, path).is_none() {
                push(path.to_string(), "no such directory".into());
            }
            continue;
        };
        if !SCANNED.contains(&extension) && extension != "json" && extension != "lock" {
            continue;
        }
        if resolve_document(root, from, path).is_none() {
            push(path.to_string(), "no such file".into());
        }
    }
}

/// A markdown link into this repo, and its anchor.
fn check_markdown_links(
    root: &Path,
    from: &str,
    line: &str,
    cache: &mut BTreeMap<String, Option<Vec<String>>>,
    push: &mut impl FnMut(String, String),
) {
    if !from.ends_with(".md") {
        return;
    }
    let base = Path::new(from).parent().unwrap_or(Path::new(""));
    for segment in line.split("](").skip(1) {
        let Some(target) = segment.split(')').next() else {
            continue;
        };
        if target.starts_with("http") || target.starts_with('#') || target.is_empty() {
            continue;
        }
        let (path, fragment) = match target.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (target, None),
        };
        // Normalized through the linking file's own directory, so a sibling link
        // inside `docs/api/` resolves the way a reader's click does.
        let joined = normalize_relative(&base.join(path));
        let resolved = joined.display().to_string();
        if !root.join(&resolved).exists() {
            push(target.to_string(), "link target does not exist".into());
            continue;
        }
        let Some(fragment) = fragment.filter(|_| resolved.ends_with(".md")) else {
            continue;
        };
        let Some(available) = document_headings(root, &resolved, cache).clone() else {
            continue;
        };
        if !available.iter().any(|h| anchor_of(h) == fragment) {
            push(target.to_string(), format!("no such anchor in {resolved}"));
        }
    }
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalize_relative(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    parts.iter().collect()
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, found);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SCANNED.contains(&e))
        {
            if let Ok(relative) = path.strip_prefix(root) {
                found.push(relative.to_path_buf());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pointer in the tree resolves.
    ///
    /// The whole point of the crate: prose citing a heading, a file, or a link that
    /// does not exist is a defect this repo cannot otherwise see, because nothing
    /// else reads prose. It fails here, on the commit that broke it, naming what to
    /// fix — rather than months later on a reader who trusted the pointer.
    #[test]
    fn every_pointer_in_the_tree_resolves() {
        let findings = check(&repo_root());
        assert!(
            findings.is_empty(),
            "{} unresolved pointer(s):\n{}",
            findings.len(),
            findings
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// A pointer at a heading that is not there is caught.
    ///
    /// Guards the checker against the failure that matters most for a lint: passing
    /// because it found nothing to look at. Asserting on a constructed miss proves
    /// the section rule can still fail, which the green tree above cannot.
    #[test]
    fn a_missing_section_is_reported() {
        let root = repo_root();
        let doc = root.join("docs/README.md");
        let text = std::fs::read_to_string(&doc).expect("docs/README.md is readable");
        let heading = "No Such Heading Exists";
        assert!(
            !text.contains(heading),
            "the fixture heading must be absent for this test to mean anything"
        );

        // Composed at runtime: a literal pointer here would be scanned by the test
        // above and reported as rot in this file, which is exactly what it is for.
        let probe = format!("See `docs/README.md` {} {heading} for this.", '\u{a7}');

        let mut found: Vec<(String, String)> = Vec::new();
        check_section_pointers(
            &root,
            "docs/probe.md",
            &probe,
            "",
            &[],
            &mut BTreeMap::new(),
            &mut |pointer, problem| found.push((pointer, problem)),
        );

        assert_eq!(found.len(), 1, "expected one finding, got {found:?}");
        assert!(found[0].1.contains("no such section"), "{:?}", found[0]);
    }
}
