import { expect, it } from "vitest";
import { utf8Window } from "./trace-utf8";
it("an incomplete multibyte suffix resumes without replacement text or lost bytes", () => {
  const bytes = new TextEncoder().encode("ABC🙂后续");
  const first = utf8Window(bytes.subarray(0, 5), 0, 0, false);
  expect(first).toEqual({ text: "ABC", start: 0, next: 3 });
  expect(utf8Window(bytes, 0, first.next, true)).toEqual({
    text: "🙂后续",
    start: 3,
    next: bytes.length,
  });
});
it("a requested offset inside a character uses the retained prefix overlap", () => {
  const bytes = new TextEncoder().encode("AB🙂CD");
  expect(utf8Window(bytes.subarray(1), 1, 4, true)).toEqual({
    text: "🙂CD",
    start: 2,
    next: bytes.length,
  });
});
it("invalid retained UTF-8 is a decode failure rather than plausible replacement prose", () => {
  expect(() => utf8Window(new Uint8Array([0xc0, 0xff]), 0, 0, true)).toThrow();
});
