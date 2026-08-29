/**
 * Grid presentation: column labels, alignment, and paging arithmetic.
 *
 * Separated from the viewer component so it is unit-testable. The column-label
 * arithmetic in particular is the kind of thing that looks obviously right and is
 * wrong at the Z→AA boundary.
 */

import type { Cell, GridPayload } from "@/lib/api-types";
import type { Translate } from "@/lib/i18n";

/**
 * Spreadsheet column label for a zero-based index: A, B, ... Z, AA, AB.
 *
 * Bijective base-26, not plain base-26: there is no zero digit, so after Z comes
 * AA rather than BA. The `- 1` is what implements that, and dropping it is the
 * classic bug here.
 */
export function columnLabel(index: number): string {
  if (index < 0 || !Number.isFinite(index)) return "";
  let remaining = Math.floor(index);
  let label = "";
  while (true) {
    label = String.fromCharCode(65 + (remaining % 26)) + label;
    if (remaining < 26) return label;
    remaining = Math.floor(remaining / 26) - 1;
  }
}

/**
 * Labels for a window that may not start at column A.
 *
 * A sheet whose used range begins at C keeps its real column letters, so a cell
 * the user finds here matches what Excel shows them.
 */
export function columnLabels(start: number, count: number): string[] {
  return Array.from({ length: count }, (_, i) => columnLabel(start + i));
}

/** Widest row in a window, since rows are ragged once trailing empties are cut. */
export function columnCount(rows: (Cell | null)[][]): number {
  return rows.reduce((widest, row) => Math.max(widest, row.length), 0);
}

/**
 * Text alignment for a cell.
 *
 * Numbers and dates right, everything else left. This is the convention every
 * spreadsheet uses, and it is most of what makes a grid scannable: aligned digits
 * let you compare magnitudes down a column without reading them.
 */
export function alignmentFor(cell: Cell | null): "left" | "right" {
  if (!cell) return "left";
  switch (cell.t) {
    case "number":
    case "date":
      return "right";
    case "formula":
      // A formula's alignment follows its result, which is numeric far more often
      // than not; `n` is present exactly when it is.
      return cell.n === undefined ? "left" : "right";
    default:
      return "left";
  }
}

/** Whether a cell should be visually marked as carrying an error. */
export function isErrorCell(cell: Cell | null): boolean {
  return cell?.t === "error";
}

/** Tooltip text, surfacing what the cell does not show inline. */
export function cellTitle(cell: Cell | null): string | undefined {
  if (!cell) return undefined;
  if (cell.f) return `= ${cell.f}`;
  // A long string is truncated by the column, so the full value belongs here.
  if (cell.v.length > 24) return cell.v;
  return undefined;
}

/** How many rows of the sheet the client currently holds. */
export function loadedThrough(grid: GridPayload): number {
  return grid.row + grid.rows.length;
}

/**
 * Row to start the next page at, or `null` when there is no next page.
 *
 * Absolute rather than relative so paging is stable if a request is retried.
 */
export function nextPageRow(grid: GridPayload): number | null {
  return grid.truncated ? loadedThrough(grid) : null;
}

/**
 * Row to start the previous page at, or `null` at the top.
 *
 * Derived from the window's own size, so the step back matches the step forward
 * even if the server clamped the page to something smaller than requested.
 */
export function previousPageRow(grid: GridPayload): number | null {
  if (grid.row === 0) return null;
  const size = Math.max(grid.rows.length, 1);
  return Math.max(0, grid.row - size);
}

/** Describe what is shown against what exists, for the viewer's status line. */
/**
 * `t` is a parameter, not a hook call: this module is pure and unit-tested, and
 * taking the translator as an argument keeps it that way while moving the English
 * plural rule into the catalogue where a translator can see it.
 */
export function describeWindow(grid: GridPayload, t: Translate): string {
  const sheet = grid.sheets[grid.sheet];
  const shown = loadedThrough(grid);
  if (!sheet) return t("grid.rowCount", { rows: shown });
  const total = sheet.exact === false ? `${sheet.rows}+` : `${sheet.rows}`;
  if (!grid.truncated && grid.row === 0) {
    return t(sheet.rows === 1 ? "grid.rowCountOne" : "grid.rowCount", { rows: sheet.rows });
  }
  return t("grid.rowRange", { from: grid.row + 1, to: shown, total });
}
