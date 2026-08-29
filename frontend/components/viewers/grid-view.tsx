"use client";

/**
 * Spreadsheets and CSV as a table.
 *
 * # Why there is no virtualization
 *
 * The server window bounds what arrives, and paging *replaces* that window rather
 * than accumulating rows, so the DOM holds one window no matter how far the user
 * pages. Measured in headless Chromium on a sticky-header table: a 200-row window
 * builds in 46 ms, and even at 2000 rows scroll cost is 0.22 ms/frame — flat
 * across a 16x range of cell counts, because scrolling a sticky-header table is
 * compositor work that does not scale with row count.
 *
 * The reasoning differs from the timeline's on both axes. A grid does not stream,
 * so the re-render count that memoization solves there is not a factor here; and
 * DOM size, which virtualization would solve, is bounded upstream by the window.
 *
 * `table-layout: fixed` on wide sheets is load-bearing: the default `auto`
 * algorithm measures every cell in the table to resolve column widths, making
 * layout cost global rather than proportional to what changed.
 *
 * The evidence that would reverse this: scroll jank within one window while
 * *idle*, the same signal `docs/frontend-architecture.md` names for the timeline.
 */

import { memo, useMemo } from "react";
import type { Cell, GridPayload } from "@/lib/api-types";
import { useT } from "@/lib/i18n";
import {
  alignmentFor,
  cellTitle,
  columnCount,
  columnLabels,
  describeWindow,
  isErrorCell,
  nextPageRow,
  previousPageRow,
} from "@/lib/grid-format";

export function GridView({
  grid,
  onSelectSheet,
  onPage,
  loading,
}: {
  grid: GridPayload;
  onSelectSheet: (index: number) => void;
  /** Jump to an absolute row. Paging replaces the window rather than growing it. */
  onPage: (row: number) => void;
  loading: boolean;
}) {
  const t = useT();
  const columns = useMemo(() => columnCount(grid.rows), [grid.rows]);
  const labels = useMemo(
    () => columnLabels(grid.column, columns),
    [grid.column, columns],
  );

  // Fixed widths keep layout linear; without them the browser measures every
  // cell in the table to resolve columns.
  const columnWidth = columns > 12 ? 120 : undefined;

  const active = grid.sheets[grid.sheet];
  const isChartsheet = active !== undefined && active.rows === 0;
  const previous = previousPageRow(grid);
  const next = nextPageRow(grid);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {grid.sheets.length > 1 && (
        <div
          role="tablist"
          aria-label={t("grid.worksheets")}
          className="flex shrink-0 gap-px overflow-x-auto border-b border-line bg-sunken px-1"
        >
          {grid.sheets.map((sheet, index) => (
            <button
              key={sheet.name}
              type="button"
              role="tab"
              aria-selected={index === grid.sheet}
              onClick={() => onSelectSheet(index)}
              title={t("grid.sheetTitle", {
                name: sheet.name,
                rows: sheet.rows,
                cols: sheet.cols,
              })}
              className={`shrink-0 rounded-t border-x border-t px-2 py-1 text-2xs focus-ring ${
                index === grid.sheet
                  ? "border-line bg-raised font-medium text-ink"
                  : "border-transparent text-ink-muted hover:bg-hover"
              }`}
            >
              {sheet.name}
            </button>
          ))}
        </div>
      )}

      {isChartsheet ? (
        <p className="p-3 text-2xs text-ink-muted">{t("grid.chartsheet")}</p>
      ) : (
        // Tinted, so the area past the last column reads as "outside the sheet"
        // rather than as empty cells. A narrow table in a dialog sized by its
        // header leaves real space to the right, and undifferentiated white there
        // looks like a grid that failed to load its data. Every spreadsheet
        // application distinguishes the two the same way.
        <div className="min-h-0 flex-1 overflow-auto overscroll-contain bg-sunken">
          <table
            className="border-collapse font-mono text-2xs"
            style={{ tableLayout: columnWidth ? "fixed" : "auto" }}
          >
            <caption className="sr-only">
              {active?.name ?? t("viewer.kind.grid")}, {describeWindow(grid, t)}
            </caption>
            <thead>
              <tr>
                {/* The row-number gutter. Sticky on both axes so it stays put
                    under the header row when scrolling diagonally. */}
                <th
                  scope="col"
                  className="sticky left-0 top-0 z-20 border border-line bg-hover px-1.5 py-0.5 text-right font-normal text-ink-muted"
                >
                  <span className="sr-only">{t("grid.row")}</span>
                </th>
                {labels.map((label) => (
                  <th
                    key={label}
                    scope="col"
                    style={columnWidth ? { width: columnWidth } : undefined}
                    className="sticky top-0 z-10 border border-line bg-hover px-1.5 py-0.5 text-left font-normal text-ink-muted"
                  >
                    {label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {grid.rows.map((row, index) => (
                <Row
                  // Absolute row number: a key tied to the window index would
                  // make every row change identity when paging.
                  key={grid.row + index}
                  cells={row}
                  columns={columns}
                  rowNumber={grid.row + index + 1}
                  isHeader={grid.hasHeaderRow && grid.row === 0 && index === 0}
                  width={columnWidth}
                />
              ))}
            </tbody>
          </table>

          {grid.rows.length === 0 && (
            <p className="p-3 text-2xs text-ink-muted">{t("grid.noRows")}</p>
          )}
        </div>
      )}

      <div className="flex shrink-0 items-center gap-2 border-t border-line bg-sunken px-2 py-1">
        <span className="flex-1 text-3xs text-ink-muted">
          {describeWindow(grid, t)}
          {loading && <span className="ml-1 text-ink-faint">{t("grid.loading")}</span>}
        </span>
        <button
          type="button"
          onClick={() => previous !== null && onPage(previous)}
          disabled={previous === null || loading}
          className="rounded border border-line bg-raised px-1.5 py-0.5 text-3xs font-medium text-ink-soft hover:bg-hover focus-ring disabled:opacity-40"
        >
          {t("grid.previous")}
        </button>
        <button
          type="button"
          onClick={() => next !== null && onPage(next)}
          disabled={next === null || loading}
          className="rounded border border-line bg-raised px-1.5 py-0.5 text-3xs font-medium text-ink-soft hover:bg-hover focus-ring disabled:opacity-40"
        >
          {t("grid.next")}
        </button>
      </div>
    </div>
  );
}

/**
 * One row.
 *
 * Memoized by cell-array identity, so paging re-renders only the rows whose
 * content actually differs rather than the whole body.
 */
const Row = memo(function Row({
  cells,
  columns,
  rowNumber,
  isHeader,
  width,
}: {
  cells: (Cell | null)[];
  columns: number;
  rowNumber: number;
  isHeader: boolean;
  width?: number;
}) {
  return (
    <tr className={isHeader ? "bg-sunken" : undefined}>
      <th
        scope="row"
        className="sticky left-0 z-10 border border-line bg-sunken px-1.5 py-0.5 text-right font-normal text-ink-faint"
      >
        {rowNumber}
      </th>
      {Array.from({ length: columns }, (_, column) => {
        const cell = cells[column] ?? null;
        return (
          <td
            key={column}
            style={width ? { width } : undefined}
            title={cellTitle(cell)}
            className={`max-w-[28rem] truncate border border-line bg-raised px-1.5 py-0.5 ${
              alignmentFor(cell) === "right" ? "text-right" : "text-left"
            } ${isErrorCell(cell) ? "text-danger" : "text-ink"} ${
              isHeader ? "bg-sunken font-medium text-ink" : ""
            }`}
          >
            {cell?.v ?? ""}
          </td>
        );
      })}
    </tr>
  );
});
