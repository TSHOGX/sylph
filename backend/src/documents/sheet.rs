//! Spreadsheet parsing via calamine: xlsx, xlsm, xlsb, xls, and ods.

use calamine::{open_workbook_auto, Data, Range, Reader};

use super::grid::{Cell, Grid, SheetInfo};
use super::{unreadable, Window};
use crate::error::{Error, Result};

/// Read one window of one sheet.
///
/// Synchronous and CPU-bound: a 50 000 × 30 sheet takes ~500–600 ms to
/// materialize. Callers run this on a blocking thread.
pub fn parse(path: &std::path::Path, window: Window) -> Result<Grid> {
    let mut workbook = open_workbook_auto(path).map_err(|_| unreadable("a spreadsheet"))?;

    let names = workbook.sheet_names().clone();
    if names.is_empty() {
        return Err(unreadable("a spreadsheet: it declares no sheets"));
    }
    if window.sheet >= names.len() {
        return Err(Error::InvalidRequest(format!(
            "This workbook has {} sheet(s), so sheet {} does not exist.",
            names.len(),
            window.sheet
        )));
    }

    // Every sheet's dimensions, not just the one being served, so the viewer can
    // populate its tab bar with row counts without a request per tab.
    //
    // This costs a parse of each sheet, which is the honest price of showing
    // truthful totals. `worksheet_range` is the only way to learn a sheet's size
    // through the public API — `Reader` exposes no dimension query.
    let mut sheets = Vec::with_capacity(names.len());
    for name in &names {
        let (rows, cols) = match workbook.worksheet_range(name) {
            Ok(range) => (range.height(), range.width()),
            // A chartsheet or macro sheet has no cell range. It is still listed,
            // because a tab silently missing from the bar is more confusing than
            // one that opens empty.
            Err(_) => (0, 0),
        };
        sheets.push(SheetInfo {
            name: name.clone(),
            rows,
            cols,
            exact: true,
        });
    }

    let name = &names[window.sheet];
    let range = workbook
        .worksheet_range(name)
        .map_err(|_| unreadable("a worksheet"))?;
    let formulas = workbook.worksheet_formula(name).ok();

    Ok(build(&range, formulas.as_ref(), &sheets, window))
}

/// Slice a materialized range into the requested window.
fn build(
    range: &Range<Data>,
    formulas: Option<&Range<String>>,
    sheets: &[SheetInfo],
    window: Window,
) -> Grid {
    // A sheet whose data starts at C5 has a used range starting at (4, 2), and
    // both offsets have to reach the client: the row offset so paging is stable,
    // the column offset so column labels are right.
    let (origin_row, origin_col) = range.start().unwrap_or((0, 0));

    let height = range.height();
    let start = window.row.min(height);
    let end = (start + window.rows).min(height);

    let mut rows = Vec::with_capacity(end.saturating_sub(start));
    for r in start..end {
        let mut row = Vec::with_capacity(range.width());
        for c in 0..range.width() {
            row.push(convert(
                range.get((r, c)),
                // Formula coordinates are relative to the formula range's own
                // origin, which matches the value range's for the same sheet.
                formulas.and_then(|f| f.get((r, c))).filter(|s| !s.is_empty()),
            ));
        }
        // Trailing empties carry no information and are most of a sparse row.
        while matches!(row.last(), Some(None)) {
            row.pop();
        }
        rows.push(row);
    }

    let has_header_row = Grid::detect_header(&rows, start == 0);

    Grid {
        sheets: sheets.to_vec(),
        sheet: window.sheet,
        row: origin_row as usize + start,
        column: origin_col as usize,
        rows,
        has_header_row,
        truncated: end < height,
    }
}

/// Map one calamine value into a wire cell.
///
/// `Empty` becomes `None` rather than an empty-string cell: sheets are mostly
/// empty, and an object per blank cell is most of the payload.
fn convert(value: Option<&Data>, formula: Option<&String>) -> Option<Cell> {
    let cell = match value? {
        Data::Empty => return None,
        Data::String(s) if s.is_empty() => return None,
        Data::String(s) => Cell::text(s.clone()),
        Data::Float(f) => Cell::number(*f),
        Data::Int(i) => Cell::number(*i as f64),
        Data::Bool(b) => Cell::bool(*b),
        // The whole reason calamine carries the `dates` feature. Without the
        // conversion the client receives 46112 and cannot recover 2026-03-31,
        // because the epoch and the 1900-leap-year quirk live in the file.
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(parsed) => Cell::date(
                if dt.is_datetime() && parsed.time() != chrono::NaiveTime::MIN {
                    parsed.format("%Y-%m-%d %H:%M").to_string()
                } else {
                    parsed.format("%Y-%m-%d").to_string()
                },
                Some(dt.as_f64()),
            ),
            // A duration, or a serial outside the representable range. Showing
            // the number beats showing nothing.
            None => Cell::number(dt.as_f64()),
        },
        Data::DateTimeIso(s) => Cell::date(s.clone(), None),
        Data::DurationIso(s) => Cell::text(s.clone()),
        Data::Error(e) => Cell::error(format!("{e:?}")),
    };

    Some(match formula {
        Some(expression) => cell.with_formula(expression.clone()),
        None => cell,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::testing;

    #[test]
    fn a_window_is_sliced_from_the_sheet_and_reports_what_remains() {
        let file = testing::xlsx_with_rows(50);
        let grid = parse(file.path(), Window::new(None, Some(10), Some(5))).unwrap();

        assert_eq!(grid.rows.len(), 5, "exactly the window asked for");
        assert_eq!(grid.row, 10, "absolute position, so paging is stable");
        assert!(grid.truncated, "35 rows remain below the window");
        assert_eq!(grid.sheets[0].rows, 50);
    }

    /// Past the end is an empty window, not an error: a client paging to the
    /// bottom should stop, not fail.
    #[test]
    fn a_window_past_the_end_is_empty_rather_than_an_error() {
        let file = testing::xlsx_with_rows(10);
        let grid = parse(file.path(), Window::new(None, Some(500), Some(5))).unwrap();
        assert!(grid.rows.is_empty());
        assert!(!grid.truncated, "nothing remains past the end");
    }

    #[test]
    fn typed_cells_survive_the_round_trip() {
        let file = testing::xlsx_typed();
        let grid = parse(file.path(), Window::new(None, None, None)).unwrap();

        let row = &grid.rows[1];
        assert_eq!(row[0].as_ref().unwrap().v, "APAC");
        let number = row[1].as_ref().unwrap();
        assert_eq!(number.v, "1200", "rendered without a spurious decimal");
        assert_eq!(number.n, Some(1200.0));
        assert_eq!(number.t, crate::documents::grid::CellType::Number);
    }

    #[test]
    fn empty_cells_are_null_rather_than_empty_strings() {
        let file = testing::xlsx_sparse();
        let grid = parse(file.path(), Window::new(None, None, None)).unwrap();
        // Row 2 has a value only in the second column.
        assert!(grid.rows[1][0].is_none());
        assert_eq!(grid.rows[1][1].as_ref().unwrap().v, "only here");
    }

    #[test]
    fn every_sheet_is_listed_with_its_own_size() {
        let file = testing::xlsx_two_sheets();
        let grid = parse(file.path(), Window::new(None, None, None)).unwrap();
        assert_eq!(grid.sheets.len(), 2);
        assert_eq!(grid.sheets[0].name, "Sales");
        assert_eq!(grid.sheets[1].name, "Notes");
    }

    #[test]
    fn a_second_sheet_can_be_selected_and_a_missing_one_is_refused() {
        let file = testing::xlsx_two_sheets();
        let grid = parse(file.path(), Window::new(Some(1), None, None)).unwrap();
        assert_eq!(grid.sheet, 1);
        assert_eq!(grid.rows[0][0].as_ref().unwrap().v, "note");

        let error = parse(file.path(), Window::new(Some(9), None, None)).unwrap_err();
        assert_eq!(error.code(), "INVALID_REQUEST");
    }

    #[test]
    fn a_sheet_whose_data_starts_away_from_a1_reports_its_origin() {
        let file = testing::xlsx_offset_origin();
        let grid = parse(file.path(), Window::new(None, None, None)).unwrap();

        // Data begins at C5. Reporting (0,0) would label the first column "A"
        // and the first row "1", mislabelling every cell in the viewer.
        assert_eq!(grid.row, 4, "row origin is 0-based, so C5 is row 4");
        assert_eq!(grid.column, 2, "column C is index 2");
        assert_eq!(grid.rows[0][0].as_ref().unwrap().v, "Region");
    }

    #[test]
    fn a_corrupt_workbook_is_a_bad_request_not_a_panic() {
        let file = testing::write_temp(b"this is not a zip archive at all");
        let error = parse(file.path(), Window::new(None, None, None)).unwrap_err();
        assert_eq!(error.code(), "INVALID_REQUEST");
        assert!(
            !error.to_string().contains(".xlsx"),
            "the message must not echo library internals or paths"
        );
    }
}
