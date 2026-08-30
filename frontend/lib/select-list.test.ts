import { describe, expect, it } from "vitest";
import {
  buildRows,
  choiceCount,
  initialIndex,
  shouldSearch,
  SELECT_SEARCH_THRESHOLD,
  type SelectOption,
} from "./select-list";

/**
 * The cursor-numbering rule, which is where the first search implementation broke.
 *
 * That version numbered rows one way while counting the cursor another, so the
 * highlight, `aria-activedescendant`, and Enter each pointed at a different row and
 * clicking a model appeared to do nothing. Every test here pins the invariant that
 * makes that impossible: a row's index is assigned once, by this module, and a row
 * that cannot be chosen has none.
 */

function choice(label: string, extra: Partial<SelectOption> = {}): SelectOption {
  return { label, checked: false, ...extra };
}

const heading = (label: string): SelectOption => ({ label, disabled: true });

describe("buildRows, unfiltered", () => {
  it("numbers only the choosable rows, so a heading does not consume an index", () => {
    const rows = buildRows([heading("Model"), choice("Fast"), choice("Thorough")], "");

    expect(rows.map((row) => row.index)).toEqual([null, 0, 1]);
  });

  it("keeps the authored order, headings included", () => {
    const rows = buildRows(
      [heading("Model"), choice("Fast"), heading("Effort"), choice("High")],
      "",
    );

    expect(rows.map((row) => row.item.label)).toEqual(["Model", "Fast", "Effort", "High"]);
  });

  it("numbers a plain action menu from zero", () => {
    const rows = buildRows(
      [{ label: "Attach files" }, { label: "Save to workspace" }],
      "",
    );

    expect(rows.map((row) => row.index)).toEqual([0, 1]);
  });
});

describe("buildRows, filtered", () => {
  const catalogue = [
    heading("Model"),
    choice("Claude Opus 5"),
    choice("Claude Sonnet 5", { hint: "Anthropic" }),
    choice("GPT-5", { hint: "OpenAI" }),
  ];

  it("drops headings, because they no longer introduce what is beneath them", () => {
    const rows = buildRows(catalogue, "claude");

    expect(rows.every((row) => row.item.disabled !== true)).toBe(true);
    expect(rows.map((row) => row.index)).toEqual([0, 1]);
  });

  it("numbers filtered rows contiguously from zero", () => {
    const rows = buildRows(catalogue, "5");

    expect(rows.map((row) => row.index)).toEqual([0, 1, 2]);
  });

  it("matches the hint, which for a model catalogue is the agent's own description", () => {
    const rows = buildRows(catalogue, "openai");

    expect(rows.map((row) => row.item.label)).toEqual(["GPT-5"]);
  });

  it("ranks a label match above a hint match", () => {
    const rows = buildRows(
      [choice("Sonnet", { hint: "cheap" }), choice("Opus", { hint: "sonnet-like" })],
      "sonnet",
    );

    expect(rows[0].item.label).toBe("Sonnet");
  });

  it("returns nothing when the query matches nothing, rather than falling back to all", () => {
    expect(buildRows(catalogue, "zzzz")).toEqual([]);
  });

  it("treats a whitespace-only query as no query", () => {
    expect(buildRows(catalogue, "   ")).toHaveLength(catalogue.length);
  });

  it("never yields a null index, so every visible row is reachable", () => {
    const rows = buildRows(catalogue, "5");

    expect(rows.every((row) => row.index !== null)).toBe(true);
  });
});

/**
 * The second line is searchable, which is the whole reason it is part of the row
 * model rather than a render callback.
 *
 * A workspace is identified by its path, and the segment a user remembers is
 * usually in the middle of it — `huawei`, `sylph` — where no leaf name carries it.
 * A `detail` the filter could not reach would render text the user can read and
 * cannot find, which is the failure mode a render escape hatch would have allowed
 * back in.
 */
describe("buildRows, the detail line", () => {
  const workspaces = [
    choice("sylph-web", { detail: "~/Workspace/sylph-web" }),
    choice("api", { detail: "~/Workspace/huawei/api" }),
    choice("notes", { detail: "~/Documents/notes" }),
  ];

  it("matches a middle path segment that no label carries", () => {
    expect(buildRows(workspaces, "huawei").map((row) => row.item.label)).toEqual(["api"]);
  });

  it("still ranks a label match above a detail match", () => {
    const rows = buildRows(
      [choice("archive", { detail: "~/Workspace/x" }), choice("x", { detail: "~/archive/y" })],
      "archive",
    );

    expect(rows[0].item.label).toBe("archive");
  });

  /**
   * Field order is a tie-break, not a dominance rule: each later field costs a
   * fixed 60, which a tighter match in a shorter string can still outweigh. So the
   * two candidates carry the *same* text, leaving position as the only variable.
   * Asserting more than this would pin a scoring accident rather than the contract.
   */
  it("ranks a detail match above an equal hint match", () => {
    const rows = buildRows(
      [choice("first", { hint: "sylph" }), choice("second", { detail: "sylph" })],
      "sylph",
    );

    expect(rows[0].item.label).toBe("second");
  });

  /**
   * `meta` is derived from the candidate rather than naming it: "3 archived" is
   * true of hundreds of rows, so admitting it would rank by a word that identifies
   * nothing. It is also a `ReactNode`, which has no text to match in the first
   * place.
   */
  it("does not match the trailing metadata", () => {
    expect(buildRows([choice("api", { meta: "12 archived" })], "archived")).toEqual([]);
  });
});

/**
 * The two fields the palette and the archive search needed, and the asymmetry
 * between them: one is matched and never rendered, the other rendered and never
 * matched.
 */
describe("buildRows, keywords and the excerpt", () => {
  it("matches a keyword the row never shows, so an alias reaches its command", () => {
    const rows = buildRows(
      [choice("Stop the running turn", { keywords: "cancel interrupt" })],
      "interrupt",
    );

    expect(rows.map((row) => row.item.label)).toEqual(["Stop the running turn"]);
  });

  /**
   * A row matched on hidden text shows the user nothing explaining why it is there,
   * so it must never displace a row whose visible text matched. Both candidates carry
   * the same string, leaving only the field it sits in as the variable.
   */
  it("ranks a visible hint match above an invisible keyword match", () => {
    const rows = buildRows(
      [choice("first", { keywords: "sylph" }), choice("second", { hint: "sylph" })],
      "sylph",
    );

    expect(rows[0].item.label).toBe("second");
  });

  /**
   * The excerpt is the *product* of a match — a full-text hit on a transcript the browser
   * never loaded — so re-ranking by it would launder the caller's ranking through a weaker
   * one. `meta` is excluded for a different reason: it names nothing.
   */
  it("does not match the excerpt", () => {
    expect(
      buildRows([choice("notes", { excerpt: "the deadlock in the supervisor" })], "deadlock"),
    ).toEqual([]);
  });
});

describe("shouldSearch", () => {
  const choices = (count: number) =>
    Array.from({ length: count }, (_, i) => choice(`Model ${i}`));

  it("counts choices, not rows, so headings cannot push a short list over", () => {
    const items = [...Array(SELECT_SEARCH_THRESHOLD).fill(null).map(() => heading("Group")), choice("One")];

    expect(choiceCount(items)).toBe(1);
    expect(shouldSearch(items, "auto")).toBe(false);
  });

  it("turns on at the threshold and stays off below it", () => {
    expect(shouldSearch(choices(SELECT_SEARCH_THRESHOLD - 1), "auto")).toBe(false);
    expect(shouldSearch(choices(SELECT_SEARCH_THRESHOLD), "auto")).toBe(true);
  });

  it("leaves an action menu alone however long it gets", () => {
    const actions = Array.from({ length: 20 }, (_, i) => ({ label: `Action ${i}` }));

    expect(shouldSearch(actions, "auto")).toBe(false);
  });

  it("is overridable in both directions", () => {
    expect(shouldSearch(choices(2), true)).toBe(true);
    expect(shouldSearch(choices(50), false)).toBe(false);
  });
});

describe("initialIndex", () => {
  it("opens on the current choice, so a long catalogue starts where the user is", () => {
    const items = [heading("Model"), choice("Fast"), choice("Thorough", { checked: true })];

    expect(initialIndex(items)).toBe(1);
  });

  it("opens at the top when nothing is checked", () => {
    expect(initialIndex([choice("Fast"), choice("Thorough")])).toBe(0);
  });

  it("counts in cursor space, not row space", () => {
    // Two headings precede the checked row, and neither takes an index.
    const items = [
      heading("Model"),
      choice("Fast"),
      heading("Effort"),
      choice("High", { checked: true }),
    ];

    expect(initialIndex(items)).toBe(1);
  });
});

/**
 * At most one row per single-select group reads as current.
 *
 * The reported symptom was two checkmarks at once: `pi` offered `deepseek-v4-pro`
 * from two providers, so the bare id was not a unique value and every producer's
 * `checked: choice.value === currentValue` — a filter — marked both. The adapter now
 * qualifies its values, and this is the second layer: one current row is a property
 * of single-select rather than of one agent's spelling, and this client renders
 * whatever an agent it has never seen reports.
 */
describe("one checked row per group", () => {
  it("keeps the first of two rows claiming to be current", () => {
    const rows = buildRows(
      [
        choice("DeepSeek V4 Pro", { hint: "ikun-deepseek", checked: true }),
        choice("DeepSeek V4 Pro", { hint: "huaweicloud", checked: true }),
      ],
      "",
    );

    expect(rows.map((row) => row.item.checked)).toEqual([true, false]);
  });

  it("gives each group between headings its own current row", () => {
    // A flattened config menu: model, then effort. Collapsing across the whole list
    // would uncheck effort's current value because model's came first.
    const rows = buildRows(
      [
        heading("Model"),
        choice("Fast", { checked: true }),
        choice("Thorough"),
        heading("Effort"),
        choice("Low"),
        choice("High", { checked: true }),
      ],
      "",
    );

    expect(rows.map((row) => row.item.checked)).toEqual([
      undefined,
      true,
      false,
      undefined,
      false,
      true,
    ]);
  });

  it("applies to a filtered list too, where headings are gone", () => {
    const rows = buildRows(
      [
        choice("DeepSeek V4 Pro", { hint: "ikun-deepseek", checked: true }),
        choice("DeepSeek V4 Pro", { hint: "huaweicloud", checked: true }),
      ],
      "deepseek",
    );

    expect(rows.filter((row) => row.item.checked === true)).toHaveLength(1);
  });

  it("leaves an already-correct list untouched, including its identity", () => {
    const items = [choice("Fast", { checked: true }), choice("Thorough")];
    const rows = buildRows(items, "");

    // Referential equality matters: callers memoize on these, so the common case
    // must not allocate a fresh array every render.
    expect(rows.map((row) => row.item)).toEqual(items);
    expect(rows[0].item).toBe(items[0]);
  });

  it("opens the cursor on the surviving current row, not the discarded duplicate", () => {
    const items = [
      choice("Other"),
      choice("DeepSeek V4 Pro", { hint: "ikun-deepseek", checked: true }),
      choice("DeepSeek V4 Pro", { hint: "huaweicloud", checked: true }),
    ];

    expect(initialIndex(items)).toBe(1);
  });

  it("does not invent a current row when none is checked", () => {
    const rows = buildRows([choice("Fast"), choice("Thorough")], "");

    expect(rows.every((row) => row.item.checked === false)).toBe(true);
  });
});
