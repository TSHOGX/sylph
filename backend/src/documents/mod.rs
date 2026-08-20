//! Parsing spreadsheets, documents, and presentations for the Artifacts pane.
//!
//! # Why this is server-side
//!
//! The browser alternative is SheetJS (`xlsx` on npm), which is frozen at
//! 0.18.5, unmaintained there, 7.5 MB unpacked, and carries two High CVEs with
//! no npm-published fix — prototype pollution (CVE-2023-30533) and ReDoS
//! (CVE-2024-22363). Both are reachable on precisely the path this feature
//! needs: parsing a file the user did not write. The frontend also has ~4 KB of
//! first-load budget, which rules out a megabyte of parser regardless of its
//! security record.
//!
//! # What crosses the wire
//!
//! Two shapes, because five formats reduce to two. A spreadsheet and a CSV are
//! both a grid of typed cells; a Word document and a PowerPoint deck are both a
//! sequence of blocks, once a slide is modelled as a section whose blocks follow
//! it. That collapse is what lets the frontend ship two viewers instead of five.
//!
//! Neither shape is Markdown or HTML. Markdown would mean escaping document
//! prose containing `*`, `_`, or `|` on the way out and re-parsing it on the way
//! in — two chances to corrupt text that was never Markdown — and would hand the
//! backend the harder job to save the frontend an easier one. HTML would
//! reintroduce an `innerHTML` path into a frontend whose Markdown renderer is
//! built specifically around not having one.
//!
//! # Untrusted input
//!
//! An agent may have downloaded any of these files, so every parser here treats
//! its input as hostile. Two independent limits, because one is only a guess:
//!
//! 1. The container is refused above [`MAX_DOCUMENT_BYTES`] before it is opened.
//! 2. A zip member is refused when its *declared* size exceeds
//!    [`MAX_PART_BYTES`], and the read is bounded anyway — the declaration comes
//!    from the archive header and an attacker writes it. A 199 KB archive
//!    declaring a 200 MB part is the shape being defended against, and it was
//!    measured, not imagined.
//!
//! The asymmetry worth knowing: limit 2 covers the parts this module walks
//! itself (docx, pptx). calamine owns its own zip reader and cannot be
//! instrumented this way, so for spreadsheets limit 1 is the only bound.

pub mod blocks;
pub mod grid;
pub mod sheet;
pub mod table;

#[cfg(test)]
pub mod testing;

use serde::Serialize;

use crate::error::{Error, Result};

/// Largest document accepted for parsing.
///
/// [`crate::api::files::MAX_PREVIEW_BYTES`] (1 MiB) is the wrong bound here: a
/// legitimate spreadsheet is routinely several MiB, and refusing those would
/// make the feature useless for the files people most want to look at. This is
/// instead sized so a parse cannot become a memory-exhaustion vector.
pub const MAX_DOCUMENT_BYTES: u64 = 25 * 1024 * 1024;

/// Largest single zip member this module will decompress.
///
/// `word/document.xml` for a large report runs a few MiB; anything past this is
/// a bomb rather than a document.
pub const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

/// Rows returned when the caller does not ask for a window.
pub const DEFAULT_ROWS: usize = 200;

/// Largest window one request may ask for.
///
/// A request beyond this is clamped rather than refused: a paging control that
/// 400s on an off-by-one is worse than one that returns a slightly smaller page,
/// and the response echoes what was actually served so the client can tell.
pub const MAX_ROWS: usize = 500;

/// Which viewer family a file belongs to.
///
/// Decided by extension, unlike `encoding` on the content endpoint which is
/// decided by the bytes. That is the right trade here: the formats are all zip
/// or delimited-text containers whose identity the extension does carry, and a
/// mislabelled file fails with `INVALID_REQUEST`, which the client answers by
/// retrying against `/files/content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Grid,
    Blocks,
}

/// The concrete container, kept alongside [`DocumentKind`].
///
/// Two fields rather than one so the UI can say "Excel workbook" while the
/// viewer switch stays a two-way branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    Xlsx,
    Xlsm,
    Xlsb,
    Xls,
    Ods,
    Csv,
    Tsv,
    Docx,
    Pptx,
}

impl DocumentFormat {
    /// Classify by extension, or `None` for a file this module does not handle.
    ///
    /// The single place extensions are mapped, so the tree, the viewer, and this
    /// endpoint cannot disagree about what a file is.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_lowercase().as_str() {
            "xlsx" => Some(Self::Xlsx),
            "xlsm" => Some(Self::Xlsm),
            "xlsb" => Some(Self::Xlsb),
            "xls" => Some(Self::Xls),
            "ods" => Some(Self::Ods),
            "csv" => Some(Self::Csv),
            "tsv" | "tab" => Some(Self::Tsv),
            "docx" => Some(Self::Docx),
            "pptx" => Some(Self::Pptx),
            _ => None,
        }
    }

    pub fn kind(self) -> DocumentKind {
        match self {
            Self::Xlsx
            | Self::Xlsm
            | Self::Xlsb
            | Self::Xls
            | Self::Ods
            | Self::Csv
            | Self::Tsv => DocumentKind::Grid,
            Self::Docx | Self::Pptx => DocumentKind::Blocks,
        }
    }

    /// Human-readable name for the viewer header.
    pub fn label(self) -> &'static str {
        match self {
            Self::Xlsx | Self::Xlsm | Self::Xlsb | Self::Xls => "Excel workbook",
            Self::Ods => "OpenDocument spreadsheet",
            Self::Csv => "CSV table",
            Self::Tsv => "TSV table",
            Self::Docx => "Word document",
            Self::Pptx => "PowerPoint presentation",
        }
    }
}

/// The window a caller asked for, already clamped to what will be served.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub sheet: usize,
    pub row: usize,
    pub rows: usize,
}

impl Window {
    /// Clamp a request into servable bounds.
    ///
    /// Zero rows would produce a response the viewer cannot render, and an
    /// unbounded request would defeat the point of windowing, so both ends are
    /// pulled into range rather than rejected.
    pub fn new(sheet: Option<usize>, row: Option<usize>, rows: Option<usize>) -> Self {
        Self {
            sheet: sheet.unwrap_or(0),
            row: row.unwrap_or(0),
            rows: rows.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS),
        }
    }
}

/// A parsed document, ready to serialize.
///
/// `kind` discriminates the payload and exactly one of `grid`/`blocks` is
/// present. A tagged union rather than two endpoints because the client should
/// not have to know which parser ran to read the result.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub kind: &'static str,
    pub format: DocumentFormat,
    /// What [`DocumentFormat::label`] returns, so the client renders a name
    /// rather than mapping the enum itself.
    pub format_label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid: Option<grid::Grid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<blocks::Blocks>,
}

impl Document {
    fn grid(format: DocumentFormat, grid: grid::Grid) -> Self {
        Self {
            kind: "grid",
            format,
            format_label: format.label(),
            grid: Some(grid),
            blocks: None,
        }
    }

    fn blocks(format: DocumentFormat, blocks: blocks::Blocks) -> Self {
        Self {
            kind: "blocks",
            format,
            format_label: format.label(),
            grid: None,
            blocks: Some(blocks),
        }
    }
}

/// Parse a file into its viewer representation.
///
/// Synchronous and CPU-bound — a 50k-row sheet takes ~600 ms — so callers must
/// run this on [`tokio::task::spawn_blocking`] rather than in a handler future.
/// Occupying a runtime worker for half a second stalls every SSE stream the
/// process is serving.
pub fn parse(path: &std::path::Path, window: Window) -> Result<Document> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let format = DocumentFormat::from_extension(extension).ok_or_else(|| {
        Error::InvalidRequest(format!(
            "Cannot open a '.{extension}' file as a document. Read it as text instead."
        ))
    })?;

    let size = std::fs::metadata(path)
        .map_err(|_| Error::FileNotFound)?
        .len();
    if size > MAX_DOCUMENT_BYTES {
        return Err(Error::InvalidRequest(format!(
            "This file is {} MiB, over the {} MiB document preview limit.",
            size / 1024 / 1024,
            MAX_DOCUMENT_BYTES / 1024 / 1024
        )));
    }

    match format {
        DocumentFormat::Csv => Ok(Document::grid(
            format,
            table::parse(path, b',', window)?,
        )),
        DocumentFormat::Tsv => Ok(Document::grid(
            format,
            table::parse(path, b'\t', window)?,
        )),
        DocumentFormat::Docx => Ok(Document::blocks(format, blocks::parse_docx(path)?)),
        DocumentFormat::Pptx => Ok(Document::blocks(format, blocks::parse_pptx(path)?)),
        _ => Ok(Document::grid(format, sheet::parse(path, window)?)),
    }
}

/// Phrase a parse failure so the reader knows whether to retry or give up.
///
/// A corrupt container is a well-formed request against an unusable file, so it
/// is the caller's file that is wrong, not the caller's request — but there is no
/// status that says that usefully to a browser, and 400 with a clear message
/// beats 422 with an obscure one. The underlying library error is deliberately
/// not forwarded: those strings carry absolute paths and internal offsets.
pub fn unreadable(what: &str) -> Error {
    Error::InvalidRequest(format!(
        "This file could not be read as {what}. It may be corrupt, encrypted, or \
         not the format its extension claims."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_one_viewer_family() {
        assert_eq!(
            DocumentFormat::from_extension("xlsx").map(DocumentFormat::kind),
            Some(DocumentKind::Grid)
        );
        assert_eq!(
            DocumentFormat::from_extension("CSV").map(DocumentFormat::kind),
            Some(DocumentKind::Grid),
            "extension matching is case-insensitive"
        );
        assert_eq!(
            DocumentFormat::from_extension("pptx").map(DocumentFormat::kind),
            Some(DocumentKind::Blocks)
        );
        assert_eq!(DocumentFormat::from_extension("rs"), None);
        assert_eq!(DocumentFormat::from_extension(""), None);
    }

    /// A window request is pulled into range rather than refused, because a
    /// paging control that errors on an off-by-one is worse than one that
    /// returns a slightly smaller page.
    #[test]
    fn windows_clamp_instead_of_failing() {
        assert_eq!(Window::new(None, None, None).rows, DEFAULT_ROWS);
        assert_eq!(Window::new(None, None, Some(0)).rows, 1);
        assert_eq!(Window::new(None, None, Some(100_000)).rows, MAX_ROWS);
        assert_eq!(Window::new(Some(3), Some(500), Some(10)).sheet, 3);
        assert_eq!(Window::new(Some(3), Some(500), Some(10)).row, 500);
    }
}
