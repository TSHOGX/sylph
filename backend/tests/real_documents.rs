//! Sanity-check the document parsers against files produced by real Office
//! tooling, rather than only the synthesized fixtures.
//!
//! Ignored by default: the fixtures come from a local spike directory that is not
//! part of the repo. Run explicitly with
//! `cargo test --test real_documents -- --ignored --nocapture`.
//!
//! The synthesized fixtures in `documents::testing` are the real test suite —
//! they run everywhere and a reviewer can read them. This exists because a
//! hand-written fixture can only encode what its author already believed, and
//! openpyxl/python-docx/python-pptx emit structures (shared strings, style
//! inheritance, namespace prefixes) that a minimal fixture does not.

use sylph_web_backend::documents::{parse, Window};

const SPIKE: &str = "/tmp/ooxml-spike/fixtures";

fn fixture(name: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(SPIKE).join(name);
    path.exists().then_some(path)
}

#[test]
#[ignore = "requires the local spike fixtures"]
fn a_real_xlsx_from_openpyxl_parses() {
    let Some(path) = fixture("sales.xlsx") else {
        eprintln!("skipped: no sales.xlsx");
        return;
    };
    let document = parse(&path, Window::new(None, None, None)).expect("parses");
    let grid = document.grid.expect("a grid payload");

    println!("sheets: {:?}", grid.sheets);
    assert_eq!(grid.sheets.len(), 2, "Sales and Notes");
    assert_eq!(grid.sheets[0].name, "Sales");

    // Shared strings, which the synthesized fixtures avoid by using inlineStr.
    assert_eq!(grid.rows[0][0].as_ref().unwrap().v, "Region");
    assert_eq!(grid.rows[1][1].as_ref().unwrap().v, "1200");

    // A real date cell, styled as a date rather than declared as one.
    let dates: Vec<&str> = grid
        .rows
        .iter()
        .flatten()
        .flatten()
        .filter(|c| c.t == sylph_web_backend::documents::grid::CellType::Date)
        .map(|c| c.v.as_str())
        .collect();
    println!("date cells: {dates:?}");
    assert!(
        dates.iter().any(|v| v.starts_with("2026-03-31")),
        "an Excel serial must arrive as a date, got {dates:?}"
    );

    // Formulas, which openpyxl writes and the fixtures do not.
    let formulas: Vec<&str> = grid
        .rows
        .iter()
        .flatten()
        .flatten()
        .filter_map(|c| c.f.as_deref())
        .collect();
    println!("formulas: {formulas:?}");
}

#[test]
#[ignore = "requires the local spike fixtures"]
fn a_real_docx_from_python_docx_parses() {
    let Some(path) = fixture("report.docx") else {
        eprintln!("skipped: no report.docx");
        return;
    };
    let document = parse(&path, Window::new(None, None, None)).expect("parses");
    let blocks = document.blocks.expect("a blocks payload");

    for block in &blocks.blocks {
        println!("{block:?}");
    }

    let headings: Vec<(&u8, &str)> = blocks
        .blocks
        .iter()
        .filter_map(|b| match b {
            sylph_web_backend::documents::blocks::Block::Heading { level, text } => {
                Some((level, text.as_str()))
            }
            _ => None,
        })
        .collect();
    assert!(
        headings.iter().any(|(l, t)| **l == 1 && *t == "Quarterly Report"),
        "real Word headings carry their level through style inheritance, got {headings:?}"
    );

    assert!(
        blocks.blocks.iter().any(|b| matches!(
            b,
            sylph_web_backend::documents::blocks::Block::Table { rows } if rows.len() == 2
        )),
        "the table survives"
    );

    // python-docx marks list items with a `ListBullet`/`ListNumber` style and no
    // `w:numPr` at all. Handling only `w:numPr` flattened all three of these to
    // paragraphs, which is the bug this test found.
    let items: Vec<(u8, bool, &str)> = blocks
        .blocks
        .iter()
        .filter_map(|b| match b {
            sylph_web_backend::documents::blocks::Block::ListItem {
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
            (0, false, "Second bullet"),
            (0, true, "Numbered one"),
        ],
        "style-marked list items must not degrade to paragraphs"
    );
}

#[test]
#[ignore = "requires the local spike fixtures"]
fn a_real_pptx_from_python_pptx_parses_in_order() {
    let Some(path) = fixture("notes.pptx") else {
        eprintln!("skipped: no notes.pptx");
        return;
    };
    let document = parse(&path, Window::new(None, None, None)).expect("parses");
    let blocks = document.blocks.expect("a blocks payload");

    for section in &blocks.sections {
        println!(
            "slide {} title={:?} notes={:?}",
            section.index, section.title, section.notes
        );
    }

    assert_eq!(blocks.sections.len(), 3);
    assert_eq!(blocks.sections[0].title.as_deref(), Some("Sylph Roadmap"));
    assert_eq!(blocks.sections[1].title.as_deref(), Some("Priorities"));
    assert!(
        blocks.sections[0]
            .notes
            .as_deref()
            .is_some_and(|n| n.contains("CVE")),
        "speaker notes from a real deck reach the payload"
    );
}
