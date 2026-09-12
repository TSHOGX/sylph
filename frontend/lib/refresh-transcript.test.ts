import { describe, expect, it } from "vitest";
import { refreshRereadsTheStore } from "./refresh-transcript";

/**
 * Which records a refresh re-parses the vendor's store for.
 *
 * **Both failures this pins are successful-looking clicks.** Answering `false` for a
 * stored record makes the control a cache invalidation again, which refetches what Sylph
 * already holds — so a transcript adopted from a terminal stops growing while the
 * terminal keeps writing, and nothing reports it. Answering `true` for a `stream` record
 * asks the server to parse a store that cannot hold `stream` fidelity, which can only
 * append at a coarser granularity than the record already has.
 *
 * Asserted on the decision rather than through the menu, because the menu renders one
 * label and closes on select whichever branch runs.
 */
describe("refreshRereadsTheStore", () => {
  it("re-reads the store for a record parsed out of one", () => {
    // The two fidelities a native store can report. An adopted session is exactly these,
    // and they are the records a re-read exists to extend.
    expect(refreshRereadsTheStore("blocks")).toBe(true);
    expect(refreshRereadsTheStore("messages")).toBe(true);
  });

  it("does not re-read a stream record, which no store can hold", () => {
    expect(refreshRereadsTheStore("stream")).toBe(false);
  });
});
