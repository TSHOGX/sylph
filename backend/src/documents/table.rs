//! Delimited text (CSV, TSV) into the same grid payload as a spreadsheet.
//!
//! Parsed here rather than in the browser for two reasons: the table viewer then
//! has exactly one input shape to handle, and a 500 MB CSV stays previewable
//! because the server reads only the window — something client-side parsing
//! cannot do, since it must first download the file.

use std::io::BufReader;

use super::grid::{Cell, Grid, SheetInfo};
use super::Window;
use crate::error::Result;

/// Read one window of a delimited file.
///
/// Streamed, never fully materialized: rows before the window are consumed and
/// dropped, and reading stops one row past its end. Memory is therefore bounded
/// by the window, not by the file.
pub fn parse(path: &std::path::Path, delimiter: u8, window: Window) -> Result<Grid> {
    let file = std::fs::File::open(path).map_err(|_| crate::error::Error::FileNotFound)?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        // Ragged rows are normal in hand-edited files, and a preview that
        // refuses to show one is less useful than a preview with a short row.
        .flexible(true)
        // The first row is data here. Whether it *looks* like a header is a
        // presentation question, answered by `Grid::detect_header`, and treating
        // it as one would drop it from the output entirely.
        .has_headers(false)
        .from_reader(BufReader::new(file));

    let mut rows = Vec::new();
    let mut total = 0usize;
    let mut cols = 0usize;
    let end = window.row.saturating_add(window.rows);

    for record in reader.records() {
        // A malformed line mid-file stops the scan rather than failing the
        // request: the rows already read are still worth showing, and a partial
        // preview beats an error page for a file the agent is mid-write on.
        let Ok(record) = record else { break };
        cols = cols.max(record.len());

        if total >= window.row && total < end {
            rows.push(
                record
                    .iter()
                    .map(|field| {
                        let field = field.trim();
                        if field.is_empty() {
                            return None;
                        }
                        // Numeric-looking fields are typed so they align right,
                        // which is most of what makes a CSV readable as a table.
                        // Everything else stays text: a leading-zero code like
                        // `007` must not become 7.
                        Some(match parse_number(field) {
                            Some(value) => Cell::number(value),
                            None => Cell::text(field),
                        })
                    })
                    .collect(),
            );
        }
        total += 1;

        // Counting the whole file to report a total would mean reading a 500 MB
        // CSV to show 200 rows. Stop one row past the window and report the
        // count as a floor via `truncated` instead.
        if total > end {
            break;
        }
    }

    let truncated = total > end;
    let has_header_row = Grid::detect_header(&rows, window.row == 0);

    Ok(Grid {
        sheets: vec![SheetInfo {
            name: "Table".to_string(),
            rows: total,
            cols,
            // The row count is where the scan stopped, not the file's true
            // length, so the client must not present it as authoritative.
            exact: !truncated,
        }],
        sheet: 0,
        row: window.row,
        column: 0,
        rows,
        has_header_row,
        truncated,
    })
}

/// Whether a field is a plain number.
///
/// Deliberately strict. `f64::from_str` accepts `inf`, `NaN`, and `1e5`, and a
/// spreadsheet column holding the word "Infinity" is text; so is a leading-zero
/// identifier, which must not lose its zeros to numeric formatting.
fn parse_number(field: &str) -> Option<f64> {
    let unsigned = field.strip_prefix(['-', '+']).unwrap_or(field);
    if unsigned.is_empty() || !unsigned.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if !unsigned
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    // `007` and `0012.5` are identifiers by convention, not quantities.
    if unsigned.len() > 1 && unsigned.starts_with('0') && !unsigned.starts_with("0.") {
        return None;
    }
    // Thousands separators are display, not value.
    field.replace(',', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::testing;

    #[test]
    fn quoted_fields_keep_their_commas_and_newlines() {
        let file = testing::write_temp(
            b"name,note\n\"Smith, Alice\",\"line one\nline two\"\nBob,plain\n",
        );
        let grid = parse(file.path(), b',', Window::new(None, None, None)).unwrap();

        assert_eq!(grid.rows[1][0].as_ref().unwrap().v, "Smith, Alice");
        assert_eq!(
            grid.rows[1][1].as_ref().unwrap().v,
            "line one\nline two",
            "an embedded newline is one cell, not two rows"
        );
        assert_eq!(grid.sheets[0].rows, 3);
    }

    #[test]
    fn tabs_delimit_a_tsv() {
        let file = testing::write_temp(b"a\tb\n1\t2\n");
        let grid = parse(file.path(), b'\t', Window::new(None, None, None)).unwrap();
        assert_eq!(grid.rows[0].len(), 2);
        assert_eq!(grid.rows[1][1].as_ref().unwrap().v, "2");
    }

    #[test]
    fn ragged_rows_are_shown_rather_than_refused() {
        let file = testing::write_temp(b"a,b,c\n1\n2,3\n");
        let grid = parse(file.path(), b',', Window::new(None, None, None)).unwrap();
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(grid.rows[1].len(), 1);
        assert_eq!(grid.sheets[0].cols, 3, "width is the widest row");
    }

    #[test]
    fn a_window_skips_earlier_rows_without_reading_the_whole_file() {
        let mut csv = String::from("h1,h2\n");
        for i in 0..1000 {
            csv.push_str(&format!("{i},value{i}\n"));
        }
        let file = testing::write_temp(csv.as_bytes());

        let grid = parse(file.path(), b',', Window::new(None, Some(500), Some(3))).unwrap();
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(grid.rows[0][1].as_ref().unwrap().v, "value499");
        assert_eq!(grid.row, 500);
        assert!(grid.truncated);
        assert!(
            !grid.sheets[0].exact,
            "the scan stopped early, so the total is a floor and must say so"
        );
    }

    /// Alignment depends on this, and so does not corrupting identifiers.
    #[test]
    fn only_plain_numbers_are_typed_as_numeric() {
        assert_eq!(parse_number("1200"), Some(1200.0));
        assert_eq!(parse_number("-3.5"), Some(-3.5));
        assert_eq!(parse_number("1,200"), Some(1200.0));
        assert_eq!(parse_number("0"), Some(0.0));
        assert_eq!(parse_number("0.75"), Some(0.75));

        assert_eq!(parse_number("007"), None, "a zero-padded code stays text");
        assert_eq!(parse_number("1e5"), None, "scientific notation is not plain");
        assert_eq!(parse_number("NaN"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number("12abc"), None);
        assert_eq!(parse_number("$5"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn an_empty_field_is_null_rather_than_an_empty_cell() {
        let file = testing::write_temp(b"a,,c\n");
        let grid = parse(file.path(), b',', Window::new(None, None, None)).unwrap();
        assert!(grid.rows[0][1].is_none());
    }
}
