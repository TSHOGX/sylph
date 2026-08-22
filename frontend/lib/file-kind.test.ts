import { describe, expect, it } from "vitest";
import {
  describeKind,
  extensionOf,
  glyphFor,
  servesRawBytes,
  sourceFor,
  viewerKindFor,
  viewerKindOf,
} from "@/lib/file-kind";

describe("extensionOf", () => {
  it("lowercases and drops the dot", () => {
    expect(extensionOf("a/b/Report.XLSX")).toBe("xlsx");
    expect(extensionOf("data.csv")).toBe("csv");
  });

  it("treats a dotfile's leading dot as part of its name", () => {
    // `.gitignore` has no extension. Reading one would classify it as a
    // "gitignore file" and, worse, could route it to a viewer by accident.
    expect(extensionOf(".gitignore")).toBe("");
    expect(extensionOf("src/.env")).toBe("");
    // A dotfile that really does carry one still reports it.
    expect(extensionOf(".eslintrc.json")).toBe("json");
  });

  it("reports nothing for a file without an extension", () => {
    expect(extensionOf("Makefile")).toBe("");
    expect(extensionOf("bin/run")).toBe("");
  });

  it("ignores dots in parent directories", () => {
    expect(extensionOf("some.dir/Makefile")).toBe("");
  });
});

describe("viewerKindFor", () => {
  it("groups every spreadsheet and delimited format as one grid family", () => {
    for (const path of [
      "a.xlsx",
      "a.xlsm",
      "a.xlsb",
      "a.xls",
      "a.ods",
      "a.csv",
      "a.tsv",
    ]) {
      expect(viewerKindFor(path), path).toBe("grid");
    }
  });

  it("groups Word and PowerPoint as one blocks family", () => {
    expect(viewerKindFor("a.docx")).toBe("blocks");
    expect(viewerKindFor("a.pptx")).toBe("blocks");
  });

  it("classifies images, pages, and PDFs", () => {
    expect(viewerKindFor("a.png")).toBe("image");
    expect(viewerKindFor("a.svg")).toBe("image");
    expect(viewerKindFor("a.html")).toBe("html");
    expect(viewerKindFor("a.htm")).toBe("html");
    expect(viewerKindFor("a.pdf")).toBe("pdf");
    expect(viewerKindFor("README.md")).toBe("markdown");
  });

  it("falls back to text, matching the server's own default", () => {
    expect(viewerKindFor("main.rs")).toBe("text");
    expect(viewerKindFor("Makefile")).toBe("text");
    expect(viewerKindFor("weird.zzz")).toBe("text");
  });
});

describe("viewerKindOf", () => {
  it("prefers the server's classification over the extension", () => {
    // The server saw the bytes; this side only sees a string.
    expect(viewerKindOf("mislabelled.txt", "grid")).toBe("grid");
  });

  it("falls back to the extension when there is no hint", () => {
    // A file opened from a tool call has no tree entry, so this path is real.
    expect(viewerKindOf("report.xlsx")).toBe("grid");
  });
});

describe("sourceFor", () => {
  it("routes parsed formats to the document endpoint", () => {
    expect(sourceFor("grid")).toBe("document");
    expect(sourceFor("blocks")).toBe("document");
  });

  it("routes browser-rendered formats to raw bytes", () => {
    expect(sourceFor("html")).toBe("raw");
    expect(sourceFor("pdf")).toBe("raw");
  });

  it("routes images and text through content", () => {
    // Images included: SVG must not be served as a renderable type from our own
    // origin, so every image travels the base64 path.
    expect(sourceFor("image")).toBe("content");
    expect(sourceFor("text")).toBe("content");
    expect(sourceFor("markdown")).toBe("content");
  });
});

describe("servesRawBytes", () => {
  it("excludes SVG, which cannot be served as a renderable type", () => {
    // An SVG from our own origin can execute script; inside an <img> it cannot.
    // The backend refuses to serve it inline, so asking for bytes yields an
    // attachment rather than an image.
    expect(servesRawBytes("icon.svg")).toBe(false);
    expect(servesRawBytes("shot.png")).toBe(true);
  });

  it("covers the formats the browser renders itself", () => {
    expect(servesRawBytes("page.html")).toBe(true);
    expect(servesRawBytes("paper.pdf")).toBe(true);
    expect(servesRawBytes("main.rs")).toBe(false);
    expect(servesRawBytes("sheet.xlsx")).toBe(false);
  });
});

describe("glyphFor", () => {
  it("distinguishes families so a file is recognizable before opening", () => {
    const glyphs = [
      glyphFor({ isDir: false, path: "a.xlsx" }),
      glyphFor({ isDir: false, path: "a.docx" }),
      glyphFor({ isDir: false, path: "a.png" }),
      glyphFor({ isDir: false, path: "a.pdf" }),
      glyphFor({ isDir: false, path: "a.html" }),
      glyphFor({ isDir: false, path: "a.rs" }),
    ];
    expect(new Set(glyphs).size).toBe(glyphs.length);
  });

  it("marks a directory regardless of its name", () => {
    // A directory called `assets.css` is still a directory.
    expect(glyphFor({ isDir: true, path: "assets.css" })).toBe(
      glyphFor({ isDir: true, path: "src" }),
    );
  });

  it("follows the server hint when present", () => {
    expect(glyphFor({ isDir: false, path: "x.bin", previewKind: "grid" })).toBe(
      glyphFor({ isDir: false, path: "y.xlsx" }),
    );
  });
});

describe("describeKind", () => {
  it("names every family", () => {
    expect(describeKind("grid")).toBe("Table");
    expect(describeKind("blocks")).toBe("Document");
    expect(describeKind("html")).toBe("Web page");
    expect(describeKind("text")).toBe("Text");
  });
});
