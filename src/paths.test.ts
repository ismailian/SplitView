import { describe, it, expect } from "vitest";
import { toShellPath, baseName, compactPath } from "./paths";

describe("toShellPath", () => {
  it("converts native Windows paths to msys form", () => {
    expect(toShellPath("C:\\Users\\john\\proj")).toBe("/c/Users/john/proj");
    expect(toShellPath("D:/work")).toBe("/d/work");
    expect(toShellPath("C:\\")).toBe("/c/");
  });

  it("leaves POSIX paths alone", () => {
    expect(toShellPath("/home/john/proj")).toBe("/home/john/proj");
    expect(toShellPath("/c/Users/john")).toBe("/c/Users/john");
  });
});

describe("baseName", () => {
  it("returns the last segment for either separator", () => {
    expect(baseName("C:\\Users\\john\\proj")).toBe("proj");
    expect(baseName("/home/john/proj/")).toBe("proj");
    expect(baseName("file.ts")).toBe("file.ts");
  });

  it("falls back to the path itself at a root", () => {
    expect(baseName("/")).toBe("/");
  });
});

describe("compactPath", () => {
  it("leaves short paths untouched", () => {
    expect(compactPath("/home/john", 32)).toBe("/home/john");
  });

  it("collapses the middle of a long path", () => {
    expect(compactPath("/home/john/dev/rust/splitview/src", 20)).toBe(
      "home/…/splitview/src",
    );
  });

  it("keeps the last segment when even the collapsed form is too long", () => {
    const p = "/home/john/a-very-very-long-directory-name-here";
    expect(compactPath(p, 20)).toBe("…/a-very-very-long-directory-name-here");
  });
});
