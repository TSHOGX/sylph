import { describe, expect, it } from "vitest";
import { mergeRequestRow, parseRequestFrame } from "@/lib/pending-requests";
import type { PendingRequest } from "@/lib/api";

/** A row with only the fields this module reads, so a schema change here is loud. */
function row(over: Partial<PendingRequest> & { id: string }): PendingRequest {
  return {
    sessionId: "s1",
    turnId: "t1",
    kind: "approval",
    title: "Run a command",
    detailJson: "{}",
    optionsJson: "[]",
    allowPersistent: false,
    status: "pending",
    createdAtMs: 1000,
    ...over,
  } as PendingRequest;
}

describe("mergeRequestRow", () => {
  it("adopts a request announced before the first read answered", () => {
    // A request can be raised before `GET /requests` lands. Dropping it here would
    // strand the agent until the 20-second poll.
    expect(mergeRequestRow(undefined, row({ id: "a" })).map((r) => r.id)).toEqual(["a"]);
  });

  it("removes a request once its announced status is no longer pending", () => {
    // The behaviour the whole change exists for: answered on a phone, retired on the
    // laptop, with no refetch and no reload.
    const shown = [row({ id: "a" }), row({ id: "b", createdAtMs: 2000 })];
    const answered = row({ id: "a", status: "resolved", chosenOption: "allow" });
    expect(mergeRequestRow(shown, answered).map((r) => r.id)).toEqual(["b"]);
  });

  it("removes a request abandoned with its agent", () => {
    // A link death is the other departure, and it reaches a client the same way. The
    // dialog must not keep offering to answer a question nobody is waiting on.
    const shown = [row({ id: "a" })];
    expect(mergeRequestRow(shown, row({ id: "a", status: "abandoned" }))).toEqual([]);
  });

  it("treats any non-pending status as a departure, including one added later", () => {
    // `status` is reconciled as a state, not replayed as a verb, so a status this
    // client has never heard of still retires the panel rather than pinning it open.
    const shown = [row({ id: "a" })];
    expect(mergeRequestRow(shown, row({ id: "a", status: "expired" }))).toEqual([]);
  });

  it("does not resurrect a departure it never saw arrive", () => {
    expect(mergeRequestRow([], row({ id: "ghost", status: "resolved" }))).toEqual([]);
  });

  it("replaces rather than duplicates when a row is announced twice", () => {
    // Arrival is announced, and the poll may also deliver it. Keyed by id so the
    // panel does not stack two copies of one question.
    const shown = [row({ id: "a", title: "old" })];
    const merged = mergeRequestRow(shown, row({ id: "a", title: "new" }));
    expect(merged).toHaveLength(1);
    expect(merged[0].title).toBe("new");
  });

  it("orders by creation so a late frame cannot jump the queue", () => {
    // The dialog renders `requests[0]` and calls it the one the agent is blocked on.
    // Appending an announced row would let a frame for an *older* request arrive
    // second and be answered first — the right panel answering the wrong question.
    const shown = [row({ id: "new", createdAtMs: 5000 })];
    const merged = mergeRequestRow(shown, row({ id: "old", createdAtMs: 1000 }));
    expect(merged.map((r) => r.id)).toEqual(["old", "new"]);
  });
});

describe("parseRequestFrame", () => {
  it("reads an announced row", () => {
    const parsed = parseRequestFrame(JSON.stringify(row({ id: "a" })));
    expect(parsed?.id).toBe("a");
  });

  it("rejects a row with no id rather than applying an unanswerable request", () => {
    // A dialog with no id has no way to answer, which is worse than one shown a
    // moment late — the invalidate that follows is the reconciler.
    expect(parseRequestFrame(JSON.stringify({ status: "pending", createdAtMs: 1 }))).toBeNull();
  });

  it("rejects a row with no status, since departure is decided by it", () => {
    expect(parseRequestFrame(JSON.stringify({ id: "a", createdAtMs: 1 }))).toBeNull();
  });

  it("rejects malformed JSON without throwing into the stream reader", () => {
    // An exception here would kill the listener for the rest of the session, taking
    // every later request with it.
    expect(parseRequestFrame("{not json")).toBeNull();
    expect(parseRequestFrame("")).toBeNull();
    expect(parseRequestFrame("null")).toBeNull();
  });
});
