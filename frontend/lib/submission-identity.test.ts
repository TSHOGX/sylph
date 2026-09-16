import { describe, expect, it } from "vitest";
import { createSubmissionIdentity } from "./submission-identity";

describe("submission identity", () => {
  it("retains an automatic import identity for manual retries until acceptance", () => {
    const identity = createSubmissionIdentity(() => "next-key");
    const payload = { environment: "machine", workdir: "/tmp/project", mode: "import_and_watch" };
    expect(identity.key(payload, "automatic-key")).toBe("automatic-key");
    expect(identity.key(payload)).toBe("automatic-key");
    identity.accepted("automatic-key");
    expect(identity.key(payload)).toBe("next-key");
  });

  it("retries an unacknowledged submission without merging a later identical message", () => {
    let sequence = 0;
    const identity = createSubmissionIdentity(() => String(++sequence));
    const first = identity.key({ text: "same message", operationRef: "one" });
    expect(identity.key({ text: "same message", operationRef: "one" })).toBe(first);
    identity.accepted(first);
    expect(identity.key({ text: "same message", operationRef: "one" })).not.toBe(first);
  });

  it("does not retire a newer submission when an older response arrives", () => {
    let sequence = 0;
    const identity = createSubmissionIdentity(() => String(++sequence));
    const old = identity.key({ text: "old" });
    const newer = identity.key({ text: "new" });
    identity.accepted(old);
    expect(identity.key({ text: "new" })).toBe(newer);
    expect(identity.key({ text: "new", operationRef: "next" })).not.toBe(newer);
  });
});
