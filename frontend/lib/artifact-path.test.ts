import { expect, test } from "vitest";
import { artifactPath } from "./artifact-path";

test("file links use their target workspace without reinterpreting unrelated absolute paths", () => {
  expect(artifactPath("/work/child", "/work/child/src/main.rs")).toBe(
    "src/main.rs",
  );
  expect(artifactPath("/work/parent", "/work/child/src/main.rs")).toBe(
    "/work/child/src/main.rs",
  );
  expect(artifactPath("/work/child", "/work/child-other/file")).toBe(
    "/work/child-other/file",
  );
  expect(artifactPath("/", "/tmp/file")).toBe("tmp/file");
  expect(artifactPath("/work/child/", "src/main.rs")).toBe("src/main.rs");
});
