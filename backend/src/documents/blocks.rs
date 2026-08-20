//! Word and PowerPoint into one block list.
//!
//! # Why one representation
//!
//! A Word document is a sequence of blocks. A PowerPoint deck is a sequence of
//! slides, each of which is a sequence of blocks. Modelling the slide as a
//! *section* over the same block vocabulary means one renderer serves both, which
//! is what keeps this affordable against the frontend's bundle budget.
//!
//! Sections are kept rather than flattened into headings. A titleless slide —
//! image-only or a continuation — would otherwise merge into the slide above it,
//! since there would be no heading to split on. That is inferring structure from
//! data that was deliberately discarded, and inferring it wrongly.
//!
//! # Why not Markdown or HTML
//!
//! Markdown would mean escaping prose containing `*`, `_`, or `|` on the way out
//! and re-parsing it on the way in: two chances to corrupt text that was never
//! Markdown, and the backend taking the harder job to spare the frontend an
//! easier one. HTML would put an `innerHTML` path back into a frontend whose
//! Markdown renderer exists specifically to avoid one.

use std::collections::HashMap;
use std::io::Read;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;

use super::{unreadable, MAX_PART_BYTES};
use crate::error::Result;

/// A run of text with inline formatting.
#[derive(Debug, Clone, Serialize)]
pub struct Span {
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
}

/// One block of document content.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    ListItem {
        level: u8,
        ordered: bool,
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
}

/// One slide, as a window into the block list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub index: usize,
    /// Speaker notes, from the slide's own `notesSlide` part.
    ///
    /// Included because notes are frequently where a deck's actual argument
    /// lives; a preview that silently dropped them would be missing the point of
    /// the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Index into `blocks` where this section's content starts.
    pub first_block: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Blocks {
    /// Slides. Empty for Word, which has no equivalent division.
    pub sections: Vec<Section>,
    pub blocks: Vec<Block>,
    pub truncated: bool,
}

/// Blocks past which a document is cut off.
///
/// A generated report can run to thousands of paragraphs, and a side panel shows
/// the first screen of them. The cap is high enough that a real document arrives
/// whole and low enough that a pathological one cannot exhaust memory.
const MAX_BLOCKS: usize = 4000;

// ------------------------------------------------------------------------ docx

/// Parse `word/document.xml` into blocks.
pub fn parse_docx(path: &std::path::Path) -> Result<Blocks> {
    let xml = read_part(path, "word/document.xml").ok_or_else(|| unreadable("a Word document"))?;
    let mut blocks = Vec::new();
    let mut reader = Reader::from_str(&xml);

    // Paragraph-level state, reset at each `w:p`.
    let mut spans: Vec<Span> = Vec::new();
    let mut heading: Option<u8> = None;
    let mut list: Option<(u8, bool)> = None;
    let mut bold = false;
    let mut italic = false;
    let mut in_run_properties = false;
    let mut preserve_space = false;

    // Table state. `w:tbl` can nest, but a preview flattens: the inner table's
    // cells land in the outer row, which reads acceptably and avoids recursion.
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;

    loop {
        match reader.read_event() {
            Err(_) => return Err(unreadable("a Word document")),
            Ok(Event::Eof) => break,

            Ok(Event::Start(tag)) => match local_name(tag.name().as_ref()) {
                b"p" => {
                    spans.clear();
                    heading = None;
                    list = None;
                }
                b"pStyle" => {
                    // A heading is a paragraph wearing a style, not its own
                    // element, so the level has to come from the style name. The
                    // same is true of a list item in generator-produced files,
                    // which often carry a `ListBullet` style and no `w:numPr`.
                    if let Some(value) = attribute(&tag, b"val") {
                        heading = heading_level(&value);
                        if heading.is_none() && list.is_none() {
                            list = list_style(&value);
                        }
                    }
                }
                b"numPr" => list = Some((0, false)),
                b"ilvl" => {
                    if let (Some(value), Some((_, ordered))) = (attribute(&tag, b"val"), list) {
                        list = Some((value.parse().unwrap_or(0), ordered));
                    }
                }
                b"rPr" => in_run_properties = true,
                // Bold and italic appear as empty marker elements inside `w:rPr`,
                // but the same names appear in style definitions elsewhere; the
                // guard keeps a style sheet from making the whole document bold.
                b"b" if in_run_properties => bold = true,
                b"i" if in_run_properties => italic = true,
                b"t" => preserve_space = attribute(&tag, b"space").as_deref() == Some("preserve"),
                b"tbl" => {
                    table = Some(Vec::new());
                    row = Vec::new();
                }
                b"tr" => row = Vec::new(),
                b"tc" => {
                    in_cell = true;
                    cell.clear();
                }
                _ => {}
            },

            Ok(Event::Empty(tag)) => match local_name(tag.name().as_ref()) {
                b"b" if in_run_properties => bold = true,
                b"i" if in_run_properties => italic = true,
                b"numPr" => list = Some((0, false)),
                b"ilvl" => {
                    if let (Some(value), Some((_, ordered))) = (attribute(&tag, b"val"), list) {
                        list = Some((value.parse().unwrap_or(0), ordered));
                    }
                }
                b"pStyle" => {
                    if let Some(value) = attribute(&tag, b"val") {
                        heading = heading_level(&value);
                        if heading.is_none() && list.is_none() {
                            list = list_style(&value);
                        }
                    }
                }
                // A line break inside a paragraph is whitespace, not a new
                // block: splitting here would fragment one sentence into two.
                b"br" | b"tab" => {
                    if in_cell {
                        cell.push(' ');
                    } else {
                        push_text(&mut spans, " ", bold, italic);
                    }
                }
                _ => {}
            },

            Ok(Event::Text(text)) => {
                let raw = text.decode().unwrap_or_default().to_string();
                let value = if preserve_space {
                    raw
                } else if raw.trim().is_empty() {
                    continue;
                } else {
                    raw
                };
                if in_cell {
                    cell.push_str(&value);
                } else {
                    push_text(&mut spans, &value, bold, italic);
                }
            }

            Ok(Event::End(tag)) => match local_name(tag.name().as_ref()) {
                b"rPr" => in_run_properties = false,
                b"r" => {
                    bold = false;
                    italic = false;
                }
                b"t" => preserve_space = false,
                b"tc" => {
                    in_cell = false;
                    row.push(cell.trim().to_string());
                }
                b"tr" => {
                    if let Some(rows) = table.as_mut() {
                        if !row.is_empty() {
                            rows.push(std::mem::take(&mut row));
                        }
                    }
                }
                b"tbl" => {
                    if let Some(rows) = table.take() {
                        if !rows.is_empty() {
                            blocks.push(Block::Table { rows });
                        }
                    }
                }
                b"p" => {
                    // Inside a table a paragraph is cell content, already
                    // accumulated, so it must not also become a block.
                    if table.is_some() {
                        continue;
                    }
                    if let Some(block) = finish_paragraph(&mut spans, heading, list) {
                        blocks.push(block);
                    }
                    if blocks.len() >= MAX_BLOCKS {
                        return Ok(Blocks {
                            sections: Vec::new(),
                            blocks,
                            truncated: true,
                        });
                    }
                }
                _ => {}
            },

            _ => {}
        }
    }

    Ok(Blocks {
        sections: Vec::new(),
        blocks,
        truncated: false,
    })
}

/// Turn accumulated spans into a block, or nothing if the paragraph was empty.
fn finish_paragraph(
    spans: &mut Vec<Span>,
    heading: Option<u8>,
    list: Option<(u8, bool)>,
) -> Option<Block> {
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    if text.trim().is_empty() {
        spans.clear();
        return None;
    }
    let block = match (heading, list) {
        (Some(level), _) => Block::Heading {
            level,
            text: text.trim().to_string(),
        },
        (None, Some((level, ordered))) => Block::ListItem {
            level,
            ordered,
            text: text.trim().to_string(),
        },
        (None, None) => Block::Paragraph {
            spans: std::mem::take(spans),
        },
    };
    spans.clear();
    Some(block)
}

/// Append text, merging into the previous span when the formatting matches.
///
/// Word splits a single styled sentence across many runs — at spell-check marks,
/// at revision boundaries — so without merging, one sentence arrives as a dozen
/// spans and the client re-joins them anyway.
fn push_text(spans: &mut Vec<Span>, text: &str, bold: bool, italic: bool) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.bold == bold && last.italic == italic => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.to_string(),
            bold,
            italic,
        }),
    }
}

/// Heading level from a paragraph style name, if it is a heading at all.
fn heading_level(style: &str) -> Option<u8> {
    let lower = style.to_lowercase();
    let digits = lower.strip_prefix("heading")?.trim_start_matches('-');
    // `Heading1` and `heading 1` are both current; `Title` is not a heading level.
    digits.trim().parse::<u8>().ok().filter(|n| (1..=6).contains(n))
}

/// List membership from a paragraph style name.
///
/// Returns `(level, ordered)`. Word marks a list item two different ways and both
/// occur in the wild:
///
/// - `w:numPr`, a numbering reference, which is what the Word UI produces; and
/// - a `ListBullet` / `ListNumber` paragraph style, which is what python-docx and
///   several other generators produce, with no `w:numPr` anywhere.
///
/// Only the first was handled at first, and a real generated document came
/// through with every bullet flattened to a paragraph. The trailing digit carries
/// the nesting depth (`ListBullet2`), 1-based in the style name against the
/// 0-based `w:ilvl`.
fn list_style(style: &str) -> Option<(u8, bool)> {
    let lower = style.to_lowercase().replace([' ', '-'], "");
    let (rest, ordered) = if let Some(rest) = lower.strip_prefix("listbullet") {
        (rest, false)
    } else if let Some(rest) = lower.strip_prefix("listnumber") {
        (rest, true)
    } else if let Some(rest) = lower.strip_prefix("listparagraph") {
        // Word's generic list style, which carries its depth in `w:numPr` when it
        // has one. Treated as a level-0 bullet so the item is not lost.
        (rest, false)
    } else {
        return None;
    };
    // `ListBullet` is depth 1 in Word's naming, so it maps to level 0.
    let level = rest.trim().parse::<u8>().unwrap_or(1).saturating_sub(1);
    Some((level.min(8), ordered))
}

// ------------------------------------------------------------------------ pptx

/// Parse a deck: presentation order, then each slide, then its notes.
pub fn parse_pptx(path: &std::path::Path) -> Result<Blocks> {
    let presentation = read_part(path, "ppt/presentation.xml")
        .ok_or_else(|| unreadable("a PowerPoint presentation"))?;
    let rels = read_part(path, "ppt/_rels/presentation.xml.rels")
        .ok_or_else(|| unreadable("a PowerPoint presentation"))?;

    let order = slide_order(&presentation, &rels)?;
    if order.is_empty() {
        return Err(unreadable(
            "a PowerPoint presentation: it lists no slides",
        ));
    }

    let mut sections = Vec::new();
    let mut blocks = Vec::new();

    for (index, part) in order.iter().enumerate() {
        let Some(xml) = read_part(path, part) else {
            // A slide named in the order but absent from the package. Skipping it
            // keeps the remaining slides readable and their numbering right.
            continue;
        };

        let first_block = blocks.len();
        let (title, body) = slide_content(&xml);
        blocks.extend(body);

        sections.push(Section {
            title,
            index,
            notes: slide_notes(path, part),
            first_block,
        });

        if blocks.len() >= MAX_BLOCKS {
            return Ok(Blocks {
                sections,
                blocks,
                truncated: true,
            });
        }
    }

    Ok(Blocks {
        sections,
        blocks,
        truncated: false,
    })
}

/// Resolve presentation order into part names.
///
/// This is the one piece of pptx handling that cannot be guessed. Slide files are
/// *not* in presentation order: `p:sldIdLst` holds the order as relationship ids,
/// and only `presentation.xml.rels` maps those to parts. Sorting filenames is
/// wrong twice over — `slide10.xml` sorts before `slide2.xml`, and deleting then
/// re-adding a slide breaks the correspondence entirely.
fn slide_order(presentation: &str, rels: &str) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut reader = Reader::from_str(presentation);
    loop {
        match reader.read_event() {
            Err(_) => return Err(unreadable("a PowerPoint presentation")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(tag)) | Ok(Event::Empty(tag)) => {
                if local_name(tag.name().as_ref()) == b"sldId" {
                    // The id attribute is namespace-prefixed (`r:id`), and the
                    // unprefixed `id` on the same element is a different value.
                    if let Some(value) = attribute_suffix(&tag, b"id") {
                        ids.push(value);
                    }
                }
            }
            _ => {}
        }
    }

    let targets = relationships(rels)?;
    Ok(ids
        .into_iter()
        .filter_map(|id| targets.get(&id).map(|target| resolve_part("ppt", target)))
        .collect())
}

/// Relationship id to target, from any `.rels` part.
fn relationships(rels: &str) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let mut reader = Reader::from_str(rels);
    loop {
        match reader.read_event() {
            Err(_) => return Err(unreadable("a relationship part")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(tag)) | Ok(Event::Empty(tag)) => {
                if local_name(tag.name().as_ref()) == b"Relationship" {
                    if let (Some(id), Some(target)) =
                        (attribute(&tag, b"Id"), attribute(&tag, b"Target"))
                    {
                        map.insert(id, target);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(map)
}

/// Resolve a relationship target against the owning part's directory.
///
/// Targets are relative to the directory of the part that declared them, so
/// `slides/slide1.xml` inside `ppt/_rels/` means `ppt/slides/slide1.xml`. A
/// leading `/` means package-absolute and must not be prefixed again. `../`
/// segments appear routinely in slide-to-notes relationships.
fn resolve_part(base: &str, target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_string();
    }
    let mut segments: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

/// A slide's title and body blocks.
///
/// The title comes from the shape whose placeholder type is `title` or
/// `ctrTitle`, not from "the first paragraph": a slide whose first shape is a
/// caption or a stray text box would otherwise be titled by it.
fn slide_content(xml: &str) -> (Option<String>, Vec<Block>) {
    let mut title = None;
    let mut blocks = Vec::new();
    let mut reader = Reader::from_str(xml);

    // Per-shape state: whether this shape is the title, and its paragraphs.
    let mut in_shape = false;
    let mut shape_is_title = false;
    let mut paragraphs: Vec<(u8, String)> = Vec::new();
    let mut level = 0u8;
    let mut text = String::new();
    let mut in_text = false;

    // Table state, for a slide carrying a `a:tbl`.
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,

            Ok(Event::Start(tag)) | Ok(Event::Empty(tag)) => {
                match local_name(tag.name().as_ref()) {
                    b"sp" => {
                        in_shape = true;
                        shape_is_title = false;
                        paragraphs.clear();
                    }
                    b"ph" => {
                        if let Some(kind) = attribute(&tag, b"type") {
                            shape_is_title = kind == "title" || kind == "ctrTitle";
                        }
                    }
                    b"p" => {
                        level = 0;
                        text.clear();
                    }
                    b"pPr" => {
                        level = attribute(&tag, b"lvl")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                    }
                    b"t" => in_text = true,
                    b"tbl" => {
                        table = Some(Vec::new());
                        row = Vec::new();
                    }
                    b"tr" => row = Vec::new(),
                    b"tc" => {
                        in_cell = true;
                        cell.clear();
                    }
                    b"br" => {
                        if in_cell {
                            cell.push(' ');
                        } else {
                            text.push(' ');
                        }
                    }
                    _ => {}
                }
            }

            Ok(Event::Text(raw)) => {
                if in_text {
                    let value = raw.decode().unwrap_or_default();
                    if in_cell {
                        cell.push_str(&value);
                    } else {
                        text.push_str(&value);
                    }
                }
            }

            Ok(Event::End(tag)) => match local_name(tag.name().as_ref()) {
                b"t" => in_text = false,
                b"tc" => {
                    in_cell = false;
                    row.push(cell.trim().to_string());
                }
                b"tr" => {
                    if let Some(rows) = table.as_mut() {
                        if !row.is_empty() {
                            rows.push(std::mem::take(&mut row));
                        }
                    }
                }
                b"tbl" => {
                    if let Some(rows) = table.take() {
                        if !rows.is_empty() {
                            blocks.push(Block::Table { rows });
                        }
                    }
                }
                b"p" => {
                    if table.is_none() && !text.trim().is_empty() {
                        paragraphs.push((level, text.trim().to_string()));
                    }
                    text.clear();
                }
                b"sp" => {
                    if in_shape {
                        if shape_is_title {
                            // Several paragraphs in one title shape are one title.
                            let joined = paragraphs
                                .iter()
                                .map(|(_, t)| t.as_str())
                                .collect::<Vec<_>>()
                                .join(" ");
                            if !joined.is_empty() {
                                title = Some(joined);
                            }
                        } else {
                            // Body text is bullets. Levels come from `a:pPr@lvl`,
                            // which is 0-based and matches the list vocabulary.
                            blocks.extend(paragraphs.drain(..).map(|(level, text)| {
                                Block::ListItem {
                                    level,
                                    ordered: false,
                                    text,
                                }
                            }));
                        }
                        paragraphs.clear();
                    }
                    in_shape = false;
                }
                _ => {}
            },

            _ => {}
        }
    }

    (title, blocks)
}

/// A slide's speaker notes, if it has a `notesSlide` relationship.
fn slide_notes(path: &std::path::Path, slide_part: &str) -> Option<String> {
    // `ppt/slides/slide1.xml` declares its relationships in
    // `ppt/slides/_rels/slide1.xml.rels`.
    let (directory, file) = slide_part.rsplit_once('/')?;
    let rels_part = format!("{directory}/_rels/{file}.rels");
    let rels = read_part(path, &rels_part)?;

    let targets = relationships(&rels).ok()?;
    let notes_part = targets
        .values()
        .find(|target| target.contains("notesSlide"))
        .map(|target| resolve_part(directory, target))?;

    let xml = read_part(path, &notes_part)?;
    let mut reader = Reader::from_str(&xml);
    let mut notes = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(tag)) if local_name(tag.name().as_ref()) == b"t" => in_text = true,
            Ok(Event::End(tag)) if local_name(tag.name().as_ref()) == b"t" => in_text = false,
            Ok(Event::End(tag)) if local_name(tag.name().as_ref()) == b"p" => notes.push(' '),
            Ok(Event::Text(raw)) if in_text => {
                notes.push_str(&raw.decode().unwrap_or_default());
            }
            _ => {}
        }
    }

    let notes = notes.trim().to_string();
    // A deck with an empty notes part is the common case; reporting `Some("")`
    // would make every slide look annotated.
    (!notes.is_empty()).then_some(notes)
}

// ----------------------------------------------------------------- zip reading

/// Read one member of an OOXML package as UTF-8.
///
/// Bounded twice, because the archive header is written by whoever produced the
/// file. A declared size over [`MAX_PART_BYTES`] is refused before any
/// decompression, and the read is capped regardless in case the declaration
/// understates the truth — verified against a 199 KB archive declaring 200 MB.
fn read_part(path: &std::path::Path, part: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let entry = archive.by_name(part).ok()?;

    if entry.size() > MAX_PART_BYTES {
        return None;
    }

    let mut text = String::new();
    entry.take(MAX_PART_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The local part of a possibly namespace-prefixed tag name (`w:p` -> `p`).
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().position(|b| *b == b':') {
        Some(index) => &name[index + 1..],
        None => name,
    }
}

/// An attribute by exact name.
fn attribute(tag: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    tag.attributes().flatten().find_map(|attribute| {
        (local_name(attribute.key.as_ref()) == name)
            .then(|| String::from_utf8_lossy(&attribute.value).to_string())
    })
}

/// An attribute matching a name with any namespace prefix.
///
/// `<p:sldId id="256" r:id="rId7"/>` carries two attributes whose local name is
/// `id`, and the one that matters is the prefixed one. Preferring a prefixed
/// match is what distinguishes them.
fn attribute_suffix(tag: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    let mut unprefixed = None;
    for attribute in tag.attributes().flatten() {
        let key = attribute.key.as_ref();
        if local_name(key) != name {
            continue;
        }
        let value = String::from_utf8_lossy(&attribute.value).to_string();
        if key.contains(&b':') {
            return Some(value);
        }
        unprefixed = Some(value);
    }
    unprefixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::testing;

    #[test]
    fn heading_styles_map_to_levels_and_other_styles_do_not() {
        assert_eq!(heading_level("Heading1"), Some(1));
        assert_eq!(heading_level("heading 3"), Some(3));
        assert_eq!(heading_level("Heading-2"), Some(2));
        assert_eq!(heading_level("Title"), None, "a title is not a level");
        assert_eq!(heading_level("BodyText"), None);
        assert_eq!(heading_level("Heading9"), None, "levels stop at 6");
    }

    #[test]
    fn relationship_targets_resolve_against_the_declaring_part() {
        assert_eq!(
            resolve_part("ppt", "slides/slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve_part("ppt", "/ppt/slides/slide1.xml"),
            "ppt/slides/slide1.xml",
            "a package-absolute target must not gain a second prefix"
        );
        assert_eq!(
            resolve_part("ppt/slides", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
    }

    #[test]
    fn docx_headings_lists_tables_and_formatting_survive() {
        let file = testing::docx();
        let parsed = parse_docx(file.path()).unwrap();

        assert!(matches!(
            &parsed.blocks[0],
            Block::Heading { level: 1, text } if text == "Quarterly Report"
        ));
        assert!(
            parsed.blocks.iter().any(|b| matches!(
                b,
                Block::Heading { level: 2, text } if text == "Findings"
            )),
            "a second-level heading keeps its level"
        );

        let list: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { level, text, .. } => Some((*level, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(list, vec![(0, "APAC led growth"), (1, "Nested detail")]);

        let table = parsed
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Table { rows } => Some(rows),
                _ => None,
            })
            .expect("the table is parsed");
        assert_eq!(table[0], vec!["Region", "Revenue"]);
        assert_eq!(table[1], vec!["APAC", "1200"]);
        assert!(!parsed.truncated);
    }

    /// A `w:br` is whitespace inside one paragraph. Treating it as a block
    /// boundary would split a sentence in half.
    #[test]
    fn a_line_break_joins_text_rather_than_splitting_the_paragraph() {
        let file = testing::docx();
        let parsed = parse_docx(file.path()).unwrap();
        let joined: Vec<String> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { spans } => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect())
                }
                _ => None,
            })
            .collect();
        assert!(
            joined.iter().any(|p: &String| p.contains("before")
                && p.contains("after")
                && !p.contains("beforeafter")),
            "got {joined:?}"
        );
    }

    #[test]
    fn xml_space_preserve_keeps_significant_whitespace() {
        let file = testing::docx();
        let parsed = parse_docx(file.path()).unwrap();
        assert!(
            parsed.blocks.iter().any(|b| matches!(
                b,
                Block::Paragraph { spans } if spans.iter().any(|s| s.text.starts_with("  "))
            )),
            "leading spaces marked xml:space=preserve must survive"
        );
    }

    /// The ordering test. The fixture's relationship ids are deliberately
    /// shuffled against its filenames, so sorting either produces a wrong answer
    /// and only `sldIdLst` through the rels is right.
    #[test]
    fn slide_order_comes_from_the_id_list_not_the_filenames() {
        let file = testing::pptx_ordered();
        let parsed = parse_pptx(file.path()).unwrap();

        assert_eq!(parsed.sections.len(), 12);
        for (index, section) in parsed.sections.iter().enumerate() {
            assert_eq!(
                section.title.as_deref(),
                Some(format!("Position {}", index + 1).as_str()),
                "slide {index} is out of presentation order"
            );
            assert_eq!(section.index, index);
        }
    }

    #[test]
    fn slides_carry_titles_levelled_bullets_and_notes() {
        let file = testing::pptx_with_notes();
        let parsed = parse_pptx(file.path()).unwrap();

        assert_eq!(parsed.sections[0].title.as_deref(), Some("Roadmap"));
        assert_eq!(
            parsed.sections[0].notes, None,
            "a slide without a notes part reports none, not an empty string"
        );

        assert_eq!(parsed.sections[1].title.as_deref(), Some("Priorities"));
        assert_eq!(
            parsed.sections[1].notes.as_deref(),
            Some("Remember the CVE research.")
        );

        // Each section points at where its blocks begin.
        let second = parsed.sections[1].first_block;
        assert!(matches!(
            &parsed.blocks[second],
            Block::ListItem { level: 0, text, .. } if text == "File preview"
        ));
        assert!(matches!(
            &parsed.blocks[second + 1],
            Block::ListItem { level: 1, text, .. } if text == "Format coverage"
        ));
    }

    /// Word marks a list item two ways and both occur in real files. Handling
    /// only `w:numPr` flattened every generator-produced bullet into a
    /// paragraph — found by testing against a python-docx file, not a fixture.
    #[test]
    fn list_items_are_recognized_from_a_style_as_well_as_from_num_pr() {
        assert_eq!(list_style("ListBullet"), Some((0, false)));
        assert_eq!(list_style("List Bullet"), Some((0, false)));
        assert_eq!(list_style("ListBullet2"), Some((1, false)));
        assert_eq!(list_style("ListNumber"), Some((0, true)));
        assert_eq!(list_style("ListNumber3"), Some((2, true)));
        assert_eq!(list_style("BodyText"), None);
        assert_eq!(list_style("Heading1"), None);
    }

    #[test]
    fn a_document_using_list_styles_still_yields_list_items() {
        let file = testing::docx_styled_lists();
        let parsed = parse_docx(file.path()).unwrap();

        let items: Vec<(u8, bool, &str)> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem {
                    level,
                    ordered,
                    text,
                } => Some((*level, *ordered, text.as_str())),
                _ => None,
            })
            .collect();

        assert_eq!(
            items,
            vec![
                (0, false, "First bullet"),
                (1, false, "Nested bullet"),
                (0, true, "Numbered one"),
            ]
        );
        // The unstyled paragraph must not become a list item.
        assert!(parsed
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph { .. })));
    }

    #[test]
    fn a_corrupt_package_is_a_bad_request() {
        let file = testing::write_temp(b"not a zip");
        assert_eq!(
            parse_docx(file.path()).unwrap_err().code(),
            "INVALID_REQUEST"
        );
        assert_eq!(
            parse_pptx(file.path()).unwrap_err().code(),
            "INVALID_REQUEST"
        );
    }

    /// A part declaring more than the cap is refused without decompressing it,
    /// which is what keeps a zip bomb from becoming an allocation.
    #[test]
    fn an_oversized_part_is_refused_rather_than_expanded() {
        let file = testing::zip_bomb("word/document.xml", 80);
        let error = parse_docx(file.path()).unwrap_err();
        assert_eq!(error.code(), "INVALID_REQUEST");
    }
}
