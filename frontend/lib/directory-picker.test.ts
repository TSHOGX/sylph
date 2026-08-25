import { describe, expect, it } from "vitest";
import { breadcrumbs, pathFromQuery } from "@/components/directory-picker";
import { shortenHome } from "@/lib/format";
import type { BrowseRoot } from "@/lib/api";

const HOME = "/Users/hsw";
const roots: BrowseRoot[] = [
  { name: "Home", path: HOME },
  { name: "Volumes", path: "/Volumes/work" },
];

/**
 * These were written against a second copy of this function that lived in the
 * picker, `abbreviate`. `lib/format`'s `shortenHome` — which five other call sites
 * already used — got the last two cases wrong, so the copies were consolidated onto
 * the tested behaviour rather than the popular one. The cases stay here because the
 * picker is where a mis-abbreviated path does real damage: it names a directory
 * that does not exist.
 */
describe("shortenHome", () => {
  it("replaces the home prefix so long paths stay readable", () => {
    expect(shortenHome("/Users/hsw/Workspace/sylph-web", HOME)).toBe("~/Workspace/sylph-web");
    expect(shortenHome(HOME, HOME)).toBe("~");
  });

  it("leaves paths outside home alone", () => {
    expect(shortenHome("/Volumes/work/project", HOME)).toBe("/Volumes/work/project");
  });

  /** A sibling sharing the prefix must not be rewritten as if it were inside. */
  it("does not abbreviate a prefix sibling", () => {
    expect(shortenHome("/Users/hsw-backup/x", HOME)).toBe("/Users/hsw-backup/x");
  });

  it("passes the path through when home is unknown", () => {
    expect(shortenHome("/Users/hsw/x", undefined)).toBe("/Users/hsw/x");
  });
});

/**
 * A typed path is a navigation target, not a filter — so the rule that decides
 * which one a query is has to be exact. Reading a plain filter as a path would
 * send a request for a directory the user never named.
 */
describe("pathFromQuery", () => {
  it("reads an absolute path as a target", () => {
    expect(pathFromQuery("/Volumes/work", HOME)).toBe("/Volumes/work");
  });

  it("expands the home shorthand, which a browser cannot do itself", () => {
    expect(pathFromQuery("~/Workspace", HOME)).toBe("/Users/hsw/Workspace");
    expect(pathFromQuery("~", HOME)).toBe(HOME);
  });

  /** Without `home` the expansion would be a guess, and a guessed path leads to a refusal. */
  it("declines the shorthand when home is unknown", () => {
    expect(pathFromQuery("~/Workspace", undefined)).toBeNull();
    expect(pathFromQuery("~", undefined)).toBeNull();
  });

  it("treats an ordinary word as a filter, not a path", () => {
    expect(pathFromQuery("sylph", HOME)).toBeNull();
    expect(pathFromQuery("", HOME)).toBeNull();
    // A relative path is still not something the backend can resolve: it takes
    // absolute paths, and there is no directory for "relative to" to mean.
    expect(pathFromQuery("Workspace/sylph", HOME)).toBeNull();
  });

  /** So `~/Workspace/` and `~/Workspace` are one path rather than two cache entries. */
  it("drops a trailing separator", () => {
    expect(pathFromQuery("~/Workspace/", HOME)).toBe("/Users/hsw/Workspace");
    expect(pathFromQuery("/Volumes/work//", HOME)).toBe("/Volumes/work");
  });

  /** The root itself is a legitimate target when it is a configured root. */
  it("keeps the filesystem root addressable", () => {
    expect(pathFromQuery("/", HOME)).toBe("/");
  });

  it("ignores surrounding whitespace, which a paste often carries", () => {
    expect(pathFromQuery("  /Volumes/work  ", HOME)).toBe("/Volumes/work");
  });
});

describe("breadcrumbs", () => {
  it("starts at the containing root and names each segment", () => {
    expect(breadcrumbs("/Users/hsw/Workspace/sylph-web", roots)).toEqual([
      { name: "Home", path: HOME },
      { name: "Workspace", path: "/Users/hsw/Workspace" },
      { name: "sylph-web", path: "/Users/hsw/Workspace/sylph-web" },
    ]);
  });

  it("yields just the root when the path is the root", () => {
    expect(breadcrumbs(HOME, roots)).toEqual([{ name: "Home", path: HOME }]);
  });

  /**
   * Crumbs above a root would be refused by the next request, so they must not
   * be offered. This is the guarantee that keeps the picker inside its bounds.
   */
  it("never offers a crumb above the root", () => {
    const crumbs = breadcrumbs("/Users/hsw/a/b", roots);
    expect(crumbs.every((crumb) => crumb.path.startsWith(HOME))).toBe(true);
    expect(crumbs.map((c) => c.path)).not.toContain("/Users");
  });

  /** With nested roots the deeper one wins, so no crumb precedes the chosen start. */
  it("anchors at the deepest containing root when roots nest", () => {
    const nested: BrowseRoot[] = [
      { name: "Home", path: HOME },
      { name: "Workspace", path: "/Users/hsw/Workspace" },
    ];
    expect(breadcrumbs("/Users/hsw/Workspace/sylph-web", nested)).toEqual([
      { name: "Workspace", path: "/Users/hsw/Workspace" },
      { name: "sylph-web", path: "/Users/hsw/Workspace/sylph-web" },
    ]);
  });

  it("falls back to the bare path when no root contains it", () => {
    expect(breadcrumbs("/etc/ssh", roots)).toEqual([{ name: "/etc/ssh", path: "/etc/ssh" }]);
  });

  /** A root that is a string prefix but not a path ancestor must not match. */
  it("does not treat a prefix sibling as contained", () => {
    expect(breadcrumbs("/Users/hsw-backup/x", roots)).toEqual([
      { name: "/Users/hsw-backup/x", path: "/Users/hsw-backup/x" },
    ]);
  });
});
