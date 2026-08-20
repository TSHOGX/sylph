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
import type { Cell, GridPayload } from "@/lib/api";
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
          aria-label="Worksheets"
          className="flex shrink-0 gap-px overflow-x-auto border-b border-gray-200 bg-gray-50 px-1"
        >
          {grid.sheets.map((sheet, index) => (
            <button
              key={sheet.name}
              type="button"
              role="tab"
              aria-selected={index === grid.sheet}
              onClick={() => onSelectSheet(index)}
              title={`${sheet.name} — ${sheet.rows} rows, ${sheet.cols} columns`}
              className={`shrink-0 rounded-t border-x border-t px-2 py-1 text-[11px] focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 ${
                index === grid.sheet
                  ? "border-gray-200 bg-white font-medium text-gray-900"
                  : "border-transparent text-gray-600 hover:bg-gray-200/60"
              }`}
            >
              {sheet.name}
            </button>
          ))}
        </div>
      )}

      {isChartsheet ? (
        <p className="p-3 text-[11px] text-gray-500">
          This sheet holds no cells — it is a chart or macro sheet rather than a
          worksheet.
        </p>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          <table
            className="border-collapse font-mono text-[11.5px]"
            style={{ tableLayout: columnWidth ? "fixed" : "auto" }}
          >
            <caption className="sr-only">
              {active?.name ?? "Table"}, {describeWindow(grid)}
            </caption>
            <thead>
              <tr>
                {/* The row-number gutter. Sticky on both axes so it stays put
                    under the header row when scrolling diagonally. */}
                <th
                  scope="col"
                  className="sticky left-0 top-0 z-20 border border-gray-200 bg-gray-100 px-1.5 py-0.5 text-right font-normal text-gray-500"
                >
                  <span className="sr-only">Row</span>
                </th>
                {labels.map((label) => (
                  <th
                    key={label}
                    scope="col"
                    style={columnWidth ? { width: columnWidth } : undefined}
                    className="sticky top-0 z-10 border border-gray-200 bg-gray-100 px-1.5 py-0.5 text-left font-normal text-gray-500"
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
            <p className="p-3 text-[11px] text-gray-500">
              No rows in this part of the sheet.
            </p>
          )}
        </div>
      )}

      <div className="flex shrink-0 items-center gap-2 border-t border-gray-200 bg-gray-50 px-2 py-1">
        <span className="flex-1 text-[10px] text-gray-500">
          {describeWindow(grid)}
          {loading && <span className="ml-1 text-gray-400">loading...</span>}
        </span>
        <button
          type="button"
          onClick={() => previous !== null && onPage(previous)}
          disabled={previous === null || loading}
          className="rounded border border-gray-200 bg-white px-1.5 py-0.5 text-[10px] font-medium text-gray-700 hover:bg-gray-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-40"
        >
          Previous
        </button>
        <button
          type="button"
          onClick={() => next !== null && onPage(next)}
          disabled={next === null || loading}
          className="rounded border border-gray-200 bg-white px-1.5 py-0.5 text-[10px] font-medium text-gray-700 hover:bg-gray-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-40"
        >
          Next
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
    <tr className={isHeader ? "bg-gray-50" : undefined}>
      <th
        scope="row"
        className="sticky left-0 z-10 border border-gray-200 bg-gray-50 px-1.5 py-0.5 text-right font-normal text-gray-400"
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
            className={`max-w-[28rem] truncate border border-gray-200 px-1.5 py-0.5 ${
              alignmentFor(cell) === "right" ? "text-right" : "text-left"
            } ${isErrorCell(cell) ? "text-red-700" : "text-gray-800"} ${
              isHeader ? "font-medium text-gray-900" : ""
            }`}
          >
            {cell?.v ?? ""}
          </td>
        );
      })}
    </tr>
  );
});
