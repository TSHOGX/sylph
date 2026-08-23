import { describe, expect, it } from "vitest";
import type { Cell, GridPayload } from "@/lib/api";
import { translate, type Translate } from "@/lib/i18n/locale";
import {
  alignmentFor,
  cellTitle,
  columnCount,
  columnLabel,
  columnLabels,
  describeWindow,
  isErrorCell,
  loadedThrough,
  nextPageRow,
  previousPageRow,
} from "@/lib/grid-format";

const text = (v: string): Cell => ({ v, t: "text" });
const num = (v: number): Cell => ({ v: String(v), t: "number", n: v });

function grid(overrides: Partial<GridPayload> = {}): GridPayload {
  return {
    sheets: [{ name: "Sheet1", rows: 100, cols: 3 }],
    sheet: 0,
    row: 0,
    column: 0,
    rows: [[text("a")], [text("b")]],
    hasHeaderRow: false,
    truncated: true,
    ...overrides,
  };
}

describe("columnLabel", () => {
  it("labels the first 26 columns A to Z", () => {
    expect(columnLabel(0)).toBe("A");
    expect(columnLabel(25)).toBe("Z");
  });

  /** The bug this function exists to avoid: bijective base-26 has no zero digit,
   *  so after Z comes AA, not BA. */
  it("crosses the Z boundary into AA rather than BA", () => {
    expect(columnLabel(26)).toBe("AA");
    expect(columnLabel(27)).toBe("AB");
    expect(columnLabel(51)).toBe("AZ");
    expect(columnLabel(52)).toBe("BA");
  });

  it("handles the second boundary too", () => {
    expect(columnLabel(701)).toBe("ZZ");
    expect(columnLabel(702)).toBe("AAA");
  });

  it("returns nothing for a nonsensical index", () => {
    expect(columnLabel(-1)).toBe("");
    expect(columnLabel(Number.NaN)).toBe("");
  });
});

describe("columnLabels", () => {
  it("keeps real column letters for a window that does not start at A", () => {
    // A sheet whose used range begins at C must label its first column C, or
    // every cell reference the user reads off the screen is wrong.
    expect(columnLabels(2, 3)).toEqual(["C", "D", "E"]);
  });
});

describe("columnCount", () => {
  it("takes the widest row, since trailing empties are trimmed per row", () => {
    expect(columnCount([[text("a")], [text("a"), text("b"), text("c")]])).toBe(3);
    expect(columnCount([])).toBe(0);
  });
});

describe("alignmentFor", () => {
  it("right-aligns quantities and left-aligns everything else", () => {
    expect(alignmentFor(num(1200))).toBe("right");
    expect(alignmentFor({ v: "2026-03-31", t: "date" })).toBe("right");
    expect(alignmentFor(text("APAC"))).toBe("left");
    expect(alignmentFor({ v: "TRUE", t: "bool" })).toBe("left");
    expect(alignmentFor(null)).toBe("left");
  });

  it("aligns a formula by its result, not by being a formula", () => {
    expect(alignmentFor({ v: "2550", t: "formula", n: 2550, f: "B2+C2" })).toBe(
      "right",
    );
    expect(alignmentFor({ v: "APAC-EMEA", t: "formula", f: "A2&A3" })).toBe("left");
  });
});

describe("cell annotations", () => {
  it("marks error cells", () => {
    expect(isErrorCell({ v: "#DIV/0!", t: "error" })).toBe(true);
    expect(isErrorCell(num(1))).toBe(false);
    expect(isErrorCell(null)).toBe(false);
  });

  it("surfaces a formula in the tooltip", () => {
    expect(cellTitle({ v: "2550", t: "formula", f: "B2+C2" })).toBe("= B2+C2");
  });

  it("surfaces a long value the column will truncate", () => {
    const long = "x".repeat(40);
    expect(cellTitle(text(long))).toBe(long);
    expect(cellTitle(text("short"))).toBeUndefined();
  });
});

describe("paging arithmetic", () => {
  it("reports how far the window reaches", () => {
    expect(loadedThrough(grid({ row: 10, rows: [[], [], []] }))).toBe(13);
  });

  it("steps forward to where the window ended", () => {
    // Absolute, so a retried request lands on the same page.
    expect(nextPageRow(grid({ row: 10, rows: [[], [], []] }))).toBe(13);
  });

  it("has no next page once the server says there is nothing more", () => {
    expect(nextPageRow(grid({ truncated: false }))).toBeNull();
  });

  it("steps back by the window's own size", () => {
    // Derived from what was served rather than what was asked for, so the step
    // back matches the step forward even when the server clamped the page.
    expect(previousPageRow(grid({ row: 200, rows: Array(200).fill([]) }))).toBe(0);
    expect(previousPageRow(grid({ row: 500, rows: Array(200).fill([]) }))).toBe(300);
  });

  it("clamps a step back at the top rather than going negative", () => {
    expect(previousPageRow(grid({ row: 50, rows: Array(200).fill([]) }))).toBe(0);
    expect(previousPageRow(grid({ row: 0, rows: Array(200).fill([]) }))).toBeNull();
  });
});

/**
 * The English translator, so these assertions stay readable strings.
 *
 * `describeWindow` and `describeBlocked` take `t` because the plural rules they
 * apply belong in the catalogue rather than in this module. Binding English here
 * keeps the test asserting on the sentence a reader sees, and it also checks the
 * keys resolve — a renamed key fails these tests, not only the UI.
 */
const t: Translate = (key, params) => translate("en", key, params);

describe("describeWindow", () => {
  it("states a whole small sheet plainly", () => {
    expect(
      describeWindow(
        grid({
          sheets: [{ name: "S", rows: 2, cols: 1 }],
          rows: [[], []],
          truncated: false,
        }),
        t,
      ),
    ).toBe("2 rows");
  });

  // The English plural rule now lives in the catalogue, so this is the assertion
  // that pins it: one row must not read "1 rows".
  it("inflects the noun for a single row", () => {
    expect(
      describeWindow(
        grid({
          sheets: [{ name: "S", rows: 1, cols: 1 }],
          rows: [[]],
          truncated: false,
        }),
        t,
      ),
    ).toBe("1 row");
  });

  it("states the range and the total when windowed", () => {
    expect(
      describeWindow(
        grid({ sheets: [{ name: "S", rows: 5000, cols: 3 }], rows: Array(200).fill([]) }),
        t,
      ),
    ).toBe("rows 1–200 of 5000");
  });

  it("marks an inexact total as a floor", () => {
    // A CSV total comes from where the scan stopped, so presenting it as
    // authoritative would be a small lie.
    expect(
      describeWindow(
        grid({
          sheets: [{ name: "S", rows: 201, cols: 3, exact: false }],
          rows: Array(200).fill([]),
        }),
        t,
      ),
    ).toBe("rows 1–200 of 201+");
  });
});
