//! Synthesized fixtures for the document parsers.
//!
//! Every fixture is built in memory from string parts rather than checked in as
//! a binary blob. Two reasons: a reviewer can see exactly what a test asserts
//! against, and a `.xlsx` in the repo is an opaque file nobody can diff. Writing
//! one costs about a screen of XML, which the OOXML minimum requires.

use std::io::{Cursor, Write};

use tempfile::NamedTempFile;
use zip::write::SimpleFileOptions;

/// Write bytes to a temp file that lives as long as the returned handle.
pub fn write_temp(bytes: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(bytes).expect("write fixture");
    file.flush().expect("flush fixture");
    file
}

/// Write bytes to a temp file carrying a specific extension.
///
/// The parsers dispatch on extension, so anything reaching `documents::parse`
/// needs a real suffix — `NamedTempFile` alone produces none.
pub fn write_temp_named(name: &str, bytes: &[u8]) -> NamedTempFile {
    let file = tempfile::Builder::new()
        .suffix(name)
        .tempfile()
        .expect("temp file");
    std::fs::write(file.path(), bytes).expect("write fixture");
    file
}

/// Assemble a zip archive from `(path, contents)` pairs.
fn archive(parts: &[(&str, String)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, body) in parts {
        zip.start_file(*name, options).expect("start zip entry");
        zip.write_all(body.as_bytes()).expect("write zip entry");
    }
    zip.finish().expect("finish zip").into_inner()
}

// ---------------------------------------------------------------- spreadsheets

/// The fixed parts every minimal xlsx needs, given a sheet list.
///
/// `sheets` is `(name, sheet_xml)`. The relationship ids are positional, which is
/// all a synthetic fixture needs; real files are not required to be.
fn xlsx(sheets: &[(&str, String)]) -> NamedTempFile {
    let sheet_entries = sheets
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            format!(
                r#"<sheet name="{name}" sheetId="{n}" r:id="rId{n}"/>"#,
                n = i + 1
            )
        })
        .collect::<String>();
    let sheet_rels = sheets
        .iter()
        .enumerate()
        .map(|(i, _)| {
            format!(
                r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#,
                n = i + 1
            )
        })
        .collect::<String>();
    let overrides = sheets
        .iter()
        .enumerate()
        .map(|(i, _)| {
            format!(
                r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
                n = i + 1
            )
        })
        .collect::<String>();

    let mut parts = vec![
        (
            "[Content_Types].xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
{overrides}
</Types>"#
            ),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "xl/workbook.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>{sheet_entries}</sheets>
</workbook>"#
            ),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{sheet_rels}</Relationships>"#
            ),
        ),
    ];

    let sheet_paths: Vec<String> = (1..=sheets.len())
        .map(|n| format!("xl/worksheets/sheet{n}.xml"))
        .collect();
    for (path, (_, body)) in sheet_paths.iter().zip(sheets) {
        parts.push((path.as_str(), body.clone()));
    }

    write_temp_named(".xlsx", &archive(&parts))
}

/// Wrap rows in the worksheet envelope.
fn worksheet(rows: String) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>{rows}</sheetData></worksheet>"#
    )
}

/// An inline string cell, avoiding a shared-strings part.
fn text_cell(reference: &str, value: &str) -> String {
    format!(r#"<c r="{reference}" t="inlineStr"><is><t>{value}</t></is></c>"#)
}

fn number_cell(reference: &str, value: &str) -> String {
    format!(r#"<c r="{reference}"><v>{value}</v></c>"#)
}

/// Column letter for a zero-based index, `A`..`Z` then `AA`.
fn column_letter(index: usize) -> String {
    let mut index = index;
    let mut label = String::new();
    loop {
        label.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    label
}

/// `count` rows of two columns: a label and its row number.
pub fn xlsx_with_rows(count: usize) -> NamedTempFile {
    let rows = (1..=count)
        .map(|r| {
            format!(
                r#"<row r="{r}">{}{}</row>"#,
                text_cell(&format!("A{r}"), &format!("row{r}")),
                number_cell(&format!("B{r}"), &r.to_string())
            )
        })
        .collect::<String>();
    xlsx(&[("Data", worksheet(rows))])
}

/// A header row over mixed text and numbers.
pub fn xlsx_typed() -> NamedTempFile {
    let rows = format!(
        r#"<row r="1">{}{}</row><row r="2">{}{}</row><row r="3">{}{}</row>"#,
        text_cell("A1", "Region"),
        text_cell("B1", "Revenue"),
        text_cell("A2", "APAC"),
        number_cell("B2", "1200"),
        text_cell("A3", "EMEA"),
        number_cell("B3", "980.5"),
    );
    xlsx(&[("Sales", worksheet(rows))])
}

/// A sheet with a hole in it, to prove empty cells serialize as null.
pub fn xlsx_sparse() -> NamedTempFile {
    let rows = format!(
        r#"<row r="1">{}{}</row><row r="2">{}</row>"#,
        text_cell("A1", "first"),
        text_cell("B1", "second"),
        text_cell("B2", "only here"),
    );
    xlsx(&[("Sparse", worksheet(rows))])
}

/// Two sheets, so tab enumeration and selection are testable.
pub fn xlsx_two_sheets() -> NamedTempFile {
    xlsx(&[
        (
            "Sales",
            worksheet(format!(r#"<row r="1">{}</row>"#, text_cell("A1", "revenue"))),
        ),
        (
            "Notes",
            worksheet(format!(r#"<row r="1">{}</row>"#, text_cell("A1", "note"))),
        ),
    ])
}

/// A sheet whose used range starts away from A1.
pub fn xlsx_offset_origin() -> NamedTempFile {
    let rows = format!(
        r#"<row r="5">{}{}</row><row r="6">{}{}</row>"#,
        text_cell("C5", "Region"),
        text_cell("D5", "Revenue"),
        text_cell("C6", "APAC"),
        number_cell("D6", "1200"),
    );
    xlsx(&[("Offset", worksheet(rows))])
}

/// A wide sheet, for column-window behaviour.
pub fn xlsx_wide(cols: usize) -> NamedTempFile {
    let header = (0..cols)
        .map(|c| text_cell(&format!("{}1", column_letter(c)), &format!("col{c}")))
        .collect::<String>();
    xlsx(&[("Wide", worksheet(format!(r#"<row r="1">{header}</row>"#)))])
}

// ------------------------------------------------------------------ documents

/// A Word document exercising headings, lists, formatting, and a table.
pub fn docx() -> NamedTempFile {
    let body = r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Quarterly Report</w:t></w:r></w:p>
<w:p><w:r><w:t>Revenue grew </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:r/><w:t>12%</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Findings</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>APAC led growth</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Nested detail</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">  spaced  </w:t></w:r></w:p>
<w:p><w:r><w:t>before</w:t></w:r><w:br/><w:r><w:t>after</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Revenue</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>APAC</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>1200</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;

    let parts = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#
                .to_string(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "word/document.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
            ),
        ),
    ];
    write_temp_named(".docx", &archive(&parts))
}

/// A Word document whose list items carry `ListBullet`/`ListNumber` *styles*
/// instead of `w:numPr`.
///
/// The convention python-docx and several other generators use. Handling only
/// `w:numPr` flattened every one of these to a plain paragraph, which is a real
/// bug a synthesized-only fixture set did not catch.
pub fn docx_styled_lists() -> NamedTempFile {
    let body = r#"<w:p><w:pPr><w:pStyle w:val="ListBullet"/></w:pPr><w:r><w:t>First bullet</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="ListBullet2"/></w:pPr><w:r><w:t>Nested bullet</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="ListNumber"/></w:pPr><w:r><w:t>Numbered one</w:t></w:r></w:p>
<w:p><w:r><w:t>Plain paragraph</w:t></w:r></w:p>"#;

    let parts = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
</Types>"#
                .to_string(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "word/document.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
            ),
        ),
    ];
    write_temp_named(".docx", &archive(&parts))
}

// -------------------------------------------------------------- presentations

/// A deck whose relationship ids deliberately do not sort with their filenames.
///
/// This is the fixture that pins slide ordering: `slide10.xml` sorts before
/// `slide2.xml` lexically, and the ids here are shuffled relative to the
/// filenames, so any implementation that sorts either one produces the wrong
/// order. Only resolving `p:sldIdLst` through the rels gets it right.
pub fn pptx_ordered() -> NamedTempFile {
    // Presentation order 1..12 maps to files in a deliberately jumbled order.
    let file_for_position = [10usize, 2, 11, 1, 12, 3, 9, 4, 8, 5, 7, 6];

    let slide_ids = file_for_position
        .iter()
        .enumerate()
        .map(|(i, _)| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 256 + i, i + 10))
        .collect::<String>();

    let rels = file_for_position
        .iter()
        .enumerate()
        .map(|(i, file)| {
            format!(
                r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{file}.xml"/>"#,
                i + 10
            )
        })
        .collect::<String>();

    let mut parts = vec![
        (
            "[Content_Types].xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
</Types>"#
                .to_string(),
        ),
        (
            "_rels/.rels".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "ppt/presentation.xml".to_string(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<p:sldIdLst>{slide_ids}</p:sldIdLst></p:presentation>"#
            ),
        ),
        (
            "ppt/_rels/presentation.xml.rels".to_string(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
            ),
        ),
    ];

    // Each slide's title states its presentation position, so a wrong order is
    // immediately legible in a failure message.
    for (position, file) in file_for_position.iter().enumerate() {
        parts.push((
            format!("ppt/slides/slide{file}.xml"),
            slide_xml(
                &format!("Position {}", position + 1),
                &[(0, format!("body of slide file {file}"))],
            ),
        ));
    }

    let borrowed: Vec<(&str, String)> = parts
        .iter()
        .map(|(name, body)| (name.as_str(), body.clone()))
        .collect();
    write_temp_named(".pptx", &archive(&borrowed))
}

/// A two-slide deck where the second slide carries speaker notes, and the first
/// slide's relationship target is package-absolute.
pub fn pptx_with_notes() -> NamedTempFile {
    let parts: Vec<(String, String)> = vec![
        (
            "[Content_Types].xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
</Types>"#
                .to_string(),
        ),
        (
            "_rels/.rels".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "ppt/presentation.xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst>
</p:presentation>"#
                .to_string(),
        ),
        (
            "ppt/_rels/presentation.xml.rels".to_string(),
            // rId2's target is package-absolute, which the resolver must accept
            // without prefixing the part's directory a second time.
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="/ppt/slides/slide1.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "ppt/slides/slide1.xml".to_string(),
            slide_xml("Roadmap", &[(0, "Ship previews".to_string())]),
        ),
        (
            "ppt/slides/slide2.xml".to_string(),
            slide_xml(
                "Priorities",
                &[
                    (0, "File preview".to_string()),
                    (1, "Format coverage".to_string()),
                ],
            ),
        ),
        (
            "ppt/slides/_rels/slide2.xml.rels".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "ppt/notesSlides/notesSlide1.xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<p:notes xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
<p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
<p:txBody><a:p><a:r><a:t>Remember the CVE research.</a:t></a:r></a:p></p:txBody>
</p:sp></p:spTree></p:cSld></p:notes>"#
                .to_string(),
        ),
    ];

    let borrowed: Vec<(&str, String)> = parts
        .iter()
        .map(|(name, body)| (name.as_str(), body.clone()))
        .collect();
    write_temp_named(".pptx", &archive(&borrowed))
}

/// One slide part: a title placeholder plus levelled body paragraphs.
fn slide_xml(title: &str, bullets: &[(u8, String)]) -> String {
    let body = bullets
        .iter()
        .map(|(level, text)| {
            if *level == 0 {
                format!(r#"<a:p><a:r><a:t>{text}</a:t></a:r></a:p>"#)
            } else {
                format!(
                    r#"<a:p><a:pPr lvl="{level}"/><a:r><a:t>{text}</a:t></a:r></a:p>"#
                )
            }
        })
        .collect::<String>();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
<p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
<p:txBody><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr>
<p:txBody>{body}</p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
    )
}

/// An archive whose single part declares an enormous decompressed size.
///
/// The shape a zip bomb takes: a few hundred KB on disk expanding to hundreds of
/// MB. Used to prove the parser refuses on the declared size and still bounds
/// the read.
pub fn zip_bomb(part: &str, uncompressed_mb: usize) -> NamedTempFile {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(part, SimpleFileOptions::default())
        .expect("start entry");
    let chunk = vec![b'0'; 1024 * 1024];
    for _ in 0..uncompressed_mb {
        zip.write_all(&chunk).expect("write entry");
    }
    let bytes = zip.finish().expect("finish zip").into_inner();
    let extension = if part.starts_with("word") {
        ".docx"
    } else {
        ".pptx"
    };
    write_temp_named(extension, &bytes)
}
