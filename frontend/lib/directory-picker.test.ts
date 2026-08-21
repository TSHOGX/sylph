import { describe, expect, it } from "vitest";
import { abbreviate, breadcrumbs } from "@/components/directory-picker";
import type { BrowseRoot } from "@/lib/api";

const HOME = "/Users/hsw";
const roots: BrowseRoot[] = [
  { name: "Home", path: HOME },
  { name: "Volumes", path: "/Volumes/work" },
];

describe("abbreviate", () => {
  it("replaces the home prefix so long paths stay readable", () => {
    expect(abbreviate("/Users/hsw/Workspace/sylph-web", HOME)).toBe("~/Workspace/sylph-web");
    expect(abbreviate(HOME, HOME)).toBe("~");
  });

  it("leaves paths outside home alone", () => {
    expect(abbreviate("/Volumes/work/project", HOME)).toBe("/Volumes/work/project");
  });

  /** A sibling sharing the prefix must not be rewritten as if it were inside. */
  it("does not abbreviate a prefix sibling", () => {
    expect(abbreviate("/Users/hsw-backup/x", HOME)).toBe("/Users/hsw-backup/x");
  });

  it("passes the path through when home is unknown", () => {
    expect(abbreviate("/Users/hsw/x", undefined)).toBe("/Users/hsw/x");
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
