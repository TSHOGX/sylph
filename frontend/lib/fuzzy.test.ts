import { describe, expect, it } from "vitest";
import { fuzzyMatch, fuzzyRank } from "@/lib/fuzzy";

/**
 * Ranking quality, not just matching.
 *
 * A palette that matches the right command but ranks it third is a palette the
 * user stops trusting, so these assert relative order rather than membership.
 */
describe("fuzzyMatch", () => {
  it("matches an empty query against anything", () => {
    expect(fuzzyMatch("anything", "")).toEqual({ score: 0, indices: [] });
  });

  it("rejects a query that is not a subsequence", () => {
    expect(fuzzyMatch("New session", "xyz")).toBeNull();
    // Order matters: the characters are all present but in the wrong sequence.
    expect(fuzzyMatch("abc", "cba")).toBeNull();
  });

  it("is case insensitive", () => {
    expect(fuzzyMatch("New Session", "NEW")).not.toBeNull();
    expect(fuzzyMatch("New Session", "new")).not.toBeNull();
  });

  it("prefers a substring match over a scattered subsequence", () => {
    const substring = fuzzyMatch("session list", "session")!;
    const scattered = fuzzyMatch("so easy, so simple, or nothing", "session");
    expect(substring).not.toBeNull();
    if (scattered) expect(substring.score).toBeGreaterThan(scattered.score);
  });

  it("prefers a match at a word boundary", () => {
    const boundary = fuzzyMatch("new session", "ses")!;
    const middle = fuzzyMatch("assesses things", "ses")!;
    expect(boundary.score).toBeGreaterThan(middle.score);
  });

  it("ranks an acronym match highly via adjacency and boundaries", () => {
    // `ns` should find "New Session" ahead of a word that merely contains n…s.
    const acronym = fuzzyMatch("New Session", "ns")!;
    const incidental = fuzzyMatch("brownish sweater", "ns")!;
    expect(acronym.score).toBeGreaterThan(incidental.score);
  });

  it("returns matched indices for highlighting", () => {
    const match = fuzzyMatch("abcdef", "ace")!;
    expect(match.indices).toEqual([0, 2, 4]);
    const direct = fuzzyMatch("xxabc", "abc")!;
    expect(direct.indices).toEqual([2, 3, 4]);
  });

  it("prefers the shorter of two candidates matching equally", () => {
    const short = fuzzyMatch("test", "test")!;
    const long = fuzzyMatch("test something much longer than needed", "test")!;
    expect(short.score).toBeGreaterThan(long.score);
  });
});

describe("fuzzyRank", () => {
  const items = [
    { title: "Deploy the backend", path: "/srv/api" },
    { title: "Unrelated", path: "/home/deploy/scripts" },
    { title: "Deployment notes", path: "/docs" },
  ];
  const fields = (item: (typeof items)[number]) => [item.title, item.path];

  it("weights an earlier field above a later one", () => {
    const ranked = fuzzyRank(items, "deploy", fields);
    // A title match outranks a path match, so the item whose *path* contains
    // "deploy" must not come first.
    expect(ranked[0].item.title).toMatch(/^Deploy/);
    expect(ranked.map((result) => result.item.title)).not.toEqual([
      "Unrelated",
      "Deploy the backend",
      "Deployment notes",
    ]);
  });

  it("drops non-matching items entirely", () => {
    const ranked = fuzzyRank(items, "zzzz", fields);
    expect(ranked).toEqual([]);
  });

  it("returns every item for an empty query, in input order", () => {
    const ranked = fuzzyRank(items, "", fields);
    expect(ranked).toHaveLength(3);
    expect(ranked[0].item.title).toBe("Deploy the backend");
  });

  it("reports which field matched, for highlighting the right one", () => {
    const ranked = fuzzyRank(items, "srv", fields);
    expect(ranked[0].field).toBe(1);
    expect(ranked[0].indices.length).toBe(3);
  });

  it("tolerates items with empty fields", () => {
    const ranked = fuzzyRank([{ title: "", path: "" }], "x", (item) => [
      item.title,
      item.path,
    ]);
    expect(ranked).toEqual([]);
  });
});
