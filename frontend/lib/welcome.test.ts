import { describe, expect, it } from "vitest";
import type { Session } from "@/lib/api";
import { recentWorkspaces, selectStartState } from "@/lib/welcome";

function session(id: string, updatedAtMs: number, workdir = "/tmp"): Session {
  return {
    id,
    origin: "sylph",
    identityMode: "sylph",
    lifecycleMode: "sylph",
    agent: "e2e",
    workdir,
    title: null,
    flagsJson: "{}",
    status: "idle",
    createdAtMs: updatedAtMs,
    updatedAtMs,
  };
}

describe("selectStartState", () => {
  it("stays pending while the listing is in flight, even with no rows yet", () => {
    // The regression: an in-flight query and an empty archive are the same empty
    // array, and showing onboarding for the former flashes "getting started" at a
    // user who has hundreds of sessions.
    expect(selectStartState(undefined, { pending: true })).toEqual({ kind: "pending" });
    expect(selectStartState([], { pending: true })).toEqual({ kind: "pending" });
  });

  it("stays pending when the query resolved to nothing at all", () => {
    // `undefined` data with `pending: false` is what a failed query looks like. A
    // failure is not evidence that no sessions exist.
    expect(selectStartState(undefined, { pending: false })).toEqual({ kind: "pending" });
  });

  it("shows the first-run guide only once an empty listing has settled", () => {
    expect(selectStartState([], { pending: false })).toEqual({ kind: "firstRun" });
  });

  it("shows recents once any session exists", () => {
    const state = selectStartState([session("a", 10)], { pending: false });
    expect(state.kind).toBe("recents");
    expect(state.kind === "recents" && state.workspaces).toHaveLength(1);
  });
});

describe("recentWorkspaces", () => {
  it("gives a directory one row however many sessions it holds", () => {
    // The reason this grouping exists: seven sessions in one directory rendered as
    // seven rows differing only by timestamp, and crowded out every other place.
    const rows = recentWorkspaces([
      session("a", 30, "/w/app"),
      session("b", 20, "/w/app"),
      session("c", 10, "/w/app"),
    ]);

    expect(rows).toHaveLength(1);
    expect(rows[0].workdir).toBe("/w/app");
    // The row opens the newest, which is the one a user means by "resume".
    expect(rows[0].session.id).toBe("a");
    expect(rows[0].otherCount).toBe(2);
  });

  it("ranks a directory by its newest session, not by how many it holds", () => {
    const rows = recentWorkspaces([
      session("old-1", 30, "/w/busy"),
      session("old-2", 20, "/w/busy"),
      session("old-3", 10, "/w/busy"),
      session("fresh", 100, "/w/quiet"),
    ]);

    expect(rows.map((r) => r.workdir)).toEqual(["/w/quiet", "/w/busy"]);
  });

  it("treats a trailing slash as the same directory", () => {
    // The backend does not guarantee a spelling, and two rows for one place is the
    // exact duplication this grouping removes.
    const rows = recentWorkspaces([session("a", 20, "/w/app/"), session("b", 10, "/w/app")]);

    expect(rows).toHaveLength(1);
    expect(rows[0].workdir).toBe("/w/app");
    expect(rows[0].otherCount).toBe(1);
  });

  it("keeps root addressable rather than collapsing it to an empty path", () => {
    expect(recentWorkspaces([session("a", 10, "/")])[0].workdir).toBe("/");
  });

  it("caps directories, not sessions", () => {
    // Twelve sessions across six directories must yield the limit in *rows*, and a
    // busy directory must not consume the whole list.
    const sessions = Array.from({ length: 6 }, (_, i) => [
      session(`a${i}`, 100 - i * 10, `/w/p${i}`),
      session(`b${i}`, 99 - i * 10, `/w/p${i}`),
    ]).flat();

    const rows = recentWorkspaces(sessions, 3);
    expect(rows.map((r) => r.workdir)).toEqual(["/w/p0", "/w/p1", "/w/p2"]);
    expect(rows.every((r) => r.otherCount === 1)).toBe(true);
  });

  it("orders correctly whatever order the wire delivered", () => {
    const rows = recentWorkspaces([
      session("mid", 20, "/w/b"),
      session("new", 30, "/w/c"),
      session("old", 10, "/w/a"),
    ]);

    expect(rows.map((r) => r.workdir)).toEqual(["/w/c", "/w/b", "/w/a"]);
  });

  it("does not reorder the array it was given", () => {
    // The input is the TanStack Query cache entry the sidebar also reads. Sorting it
    // in place would reorder the sidebar's grouping as a side effect.
    const input = [session("a", 1), session("b", 3), session("c", 2)];
    recentWorkspaces(input);
    expect(input.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });

  it("handles an empty list", () => {
    expect(recentWorkspaces([])).toEqual([]);
  });
});
