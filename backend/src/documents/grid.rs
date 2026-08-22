//! The grid payload: a window of typed cells, shared by spreadsheets and CSV.
//!
//! One shape for both so the frontend ships one table viewer. A CSV could be
//! parsed in the browser, but routing it through here costs a few lines against
//! the `csv` crate and buys two things: the viewer has exactly one input shape,
//! and a 500 MB CSV is previewable because the server slices it — which
//! client-side parsing could never do.

use serde::Serialize;

/// One sheet's identity and true size.
///
/// The dimensions describe the *sheet*, not the window served, so the viewer can
/// say "200 of 50,000 rows" rather than implying it has everything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetInfo {
    pub name: String,
    pub rows: usize,
    pub cols: usize,
    /// False when the totals were counted rather than read from the file.
    ///
    /// An xlsx worksheet *may* declare its used range in a `<dimension>` element
    /// but is not required to. When it is absent the count comes from scanning,
    /// and a client that presents a scanned total as authoritative would be
    /// lying by a little; this lets it hedge.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,
}

/// What kind of value a cell holds.
///
/// This exists to drive alignment — numbers and dates right, text left — which is
/// most of what makes a grid readable rather than a wall of strings. It is not a
/// full type system and does not try to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CellType {
    Text,
    Number,
    Bool,
    Date,
    Formula,
    Error,
}

/// One non-empty cell.
///
/// `v` is rendered server-side because that is where the format information
/// lives: calamine hands back a date as an Excel serial and the number format
/// separately, and a client receiving `46112` cannot recover `2026-03-31`.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    /// The string to display.
    pub v: String,
    pub t: CellType,
    /// Raw numeric value, so a client need not re-parse a formatted string to
    /// sort or chart it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    /// The formula expression, when this cell is computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f: Option<String>,
}

impl Cell {
    pub fn text(value: impl Into<String>) -> Self {
        Self {
            v: value.into(),
            t: CellType::Text,
            n: None,
            f: None,
        }
    }

    pub fn number(value: f64) -> Self {
        Self {
            // Integral floats render as `1200`, not `1200.0`: the latter is
            // calamine's Rust spelling, not what the spreadsheet showed.
            v: if value.fract() == 0.0 && value.abs() < 1e15 {
                format!("{}", value as i64)
            } else {
                value.to_string()
            },
            t: CellType::Number,
            n: Some(value),
            f: None,
        }
    }

    pub fn bool(value: bool) -> Self {
        Self {
            v: if value { "TRUE" } else { "FALSE" }.to_string(),
            t: CellType::Bool,
            n: None,
            f: None,
        }
    }

    /// A date, already rendered ISO-8601 by the caller.
    pub fn date(iso: impl Into<String>, serial: Option<f64>) -> Self {
        Self {
            v: iso.into(),
            t: CellType::Date,
            n: serial,
            f: None,
        }
    }

    pub fn error(value: impl Into<String>) -> Self {
        Self {
            v: value.into(),
            t: CellType::Error,
            n: None,
            f: None,
        }
    }

    /// Attach a formula to an already-typed cell.
    ///
    /// The computed value stays in `v` because that is what the spreadsheet
    /// displays; the expression rides alongside. A preview that showed `=B2+C2`
    /// instead of `2550` would be showing the wrong one of the two.
    pub fn with_formula(mut self, formula: impl Into<String>) -> Self {
        self.f = Some(formula.into());
        self.t = CellType::Formula;
        self
    }
}

/// A window of one sheet.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grid {
    pub sheets: Vec<SheetInfo>,
    /// Index of the sheet actually served.
    pub sheet: usize,
    /// First row of the window, absolute within the sheet.
    pub row: usize,
    /// Column the window starts at.
    ///
    /// Non-zero when the sheet's used range does not begin at column A. Without
    /// it a viewer would label a sheet whose data starts at C5 as though it
    /// started at A1, mislabelling every column.
    pub column: usize,
    /// The window. A `None` cell is empty, which is most of a sparse sheet and
    /// therefore most of the payload saved by not sending an object for it.
    pub rows: Vec<Vec<Option<Cell>>>,
    /// Whether the first row of the *sheet* looks like a header.
    ///
    /// Advisory and heuristic: all-text across a row whose successor is not all
    /// text. The viewer uses it to decide on sticky styling, and being wrong
    /// costs an unhelpfully bold row, not a misreading.
    pub has_header_row: bool,
    /// True when the sheet holds rows past this window.
    pub truncated: bool,
}

impl Grid {
    /// Guess whether row zero is a header.
    ///
    /// Only meaningful when the window starts at the top of the sheet; a window
    /// scrolled into the middle has no header to detect and reports false.
    pub fn detect_header(rows: &[Vec<Option<Cell>>], starts_at_top: bool) -> bool {
        if !starts_at_top || rows.len() < 2 {
            return false;
        }
        let all_text = |row: &Vec<Option<Cell>>| {
            let mut seen = false;
            for cell in row.iter().flatten() {
                seen = true;
                if cell.t != CellType::Text {
                    return false;
                }
            }
            seen
        };
        // A header is text over a body that is not all text. Two all-text rows
        // are just as likely to be data, so this deliberately does not fire.
        all_text(&rows[0]) && !all_text(&rows[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integral_numbers_render_without_a_decimal_point() {
        assert_eq!(Cell::number(1200.0).v, "1200");
        assert_eq!(Cell::number(980.5).v, "980.5");
        assert_eq!(Cell::number(-3.0).v, "-3");
        // The raw value survives regardless of how it was rendered.
        assert_eq!(Cell::number(980.5).n, Some(980.5));
    }

    #[test]
    fn a_formula_keeps_its_computed_value_and_gains_the_expression() {
        let cell = Cell::number(2550.0).with_formula("B2+C2");
        assert_eq!(cell.v, "2550", "the display value is what Excel showed");
        assert_eq!(cell.f.as_deref(), Some("B2+C2"));
        assert_eq!(cell.t, CellType::Formula);
    }

    #[test]
    fn a_text_row_over_a_numeric_row_reads_as_a_header() {
        let rows = vec![
            vec![Some(Cell::text("Region")), Some(Cell::text("Revenue"))],
            vec![Some(Cell::text("APAC")), Some(Cell::number(1200.0))],
        ];
        assert!(Grid::detect_header(&rows, true));
        // The same rows partway down a sheet have no header above them.
        assert!(!Grid::detect_header(&rows, false));
    }

    #[test]
    fn two_text_rows_are_not_assumed_to_be_a_header() {
        let rows = vec![
            vec![Some(Cell::text("a")), Some(Cell::text("b"))],
            vec![Some(Cell::text("c")), Some(Cell::text("d"))],
        ];
        assert!(!Grid::detect_header(&rows, true));
    }

    #[test]
    fn an_empty_sheet_has_no_header() {
        assert!(!Grid::detect_header(&[], true));
        assert!(!Grid::detect_header(&[vec![Some(Cell::text("only"))]], true));
    }
}
