import { describe, expect, it } from "vitest";
import { requestDetail, requestOptions, type PendingRequest } from "./api-types";

function request(patch: Partial<PendingRequest> = {}): PendingRequest { return { id: "request-one", kind: "approval", title: "Run this command", detail: { command: "build project" }, options: [{ id: "allow", label: "Allow once" }, { id: "deny", label: "Deny" }], allowPersistent: true, ...patch }; }

describe("declared request controls", () => {
  it("keeps an object proposal intact for explicit editing", () => { expect(requestDetail(request())).toEqual({ command: "build project" }); });
  it("does not treat an array or scalar as an editable object", () => { for (const detail of [null, ["argument"], 42, "raw detail"]) expect(requestDetail(request({ detail }))).toEqual({}); });
  it("keeps the declared choice order and labels", () => { expect(requestOptions(request())).toEqual([{ id: "allow", label: "Allow once" }, { id: "deny", label: "Deny" }]); });
  it("omits a persistent grant when the current request prohibits it", () => { expect(requestOptions(request({ allowPersistent: false, options: [{ id: "lasting", label: "Always", persistent: true }, { id: "once", label: "Once" }] }))).toEqual([{ id: "once", label: "Once" }]); });
  it("does not fabricate answer options when no permitted choice exists", () => { expect(requestOptions(request({ allowPersistent: false, options: [{ id: "lasting", label: "Always", persistent: true }] }))).toEqual([]); expect(requestOptions(request({ options: [] }))).toEqual([]); });
});
