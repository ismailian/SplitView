import { describe, it, expect } from "vitest";
import {
  toShellPath,
  baseName,
  compactPath,
  parseOsc7,
  promptDir,
  resolvePath,
} from "./paths";

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

describe("promptDir", () => {
  // The expected values here are what bash actually prints from __sv_compact /
  // __sv_tail_segs in src-tauri/src/shell.rs; this mirror feeds the preview.
  it("shows the last n segments, counted from the end", () => {
    const p = "/d/projects/project-x/mobile-app";
    expect(promptDir(p, 1)).toBe("mobile-app");
    expect(promptDir(p, 2)).toBe("project-x/mobile-app");
    expect(promptDir(p, 3)).toBe("projects/project-x/mobile-app");
  });

  it("shows the whole path when it has no more segments than asked for", () => {
    const p = "/d/projects/project-x/mobile-app";
    expect(promptDir(p, 4)).toBe(p);
    expect(promptDir(p, 9)).toBe(p);
    expect(promptDir("~", 2)).toBe("~");
  });

  it("elides the middle of a deep path when segments is 0", () => {
    expect(promptDir("~/projects/project-x/mobile-app", 0)).toBe(
      "~/projects/.../mobile-app",
    );
    expect(promptDir("/d/projects/project-x/mobile-app", 0)).toBe(
      "/d/.../mobile-app",
    );
  });

  it("leaves a shallow path alone when segments is 0", () => {
    expect(promptDir("~/projects", 0)).toBe("~/projects");
    expect(promptDir("/d/projects", 0)).toBe("/d/projects");
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

describe("resolvePath", () => {
  it("joins a relative path onto the shell's directory", () => {
    expect(resolvePath("/c/Users/me/proj", "src/main.ts")).toBe(
      "/c/Users/me/proj/src/main.ts",
    );
    expect(resolvePath("/home/me/", "a b.txt")).toBe("/home/me/a b.txt");
  });

  it("leaves absolute paths alone", () => {
    expect(resolvePath("/home/me", "/etc/hosts")).toBe("/etc/hosts");
    expect(resolvePath("/c/x", "C:\\Users\\me")).toBe("C:\\Users\\me");
  });

  it("has nothing to join to before the shell reports a directory", () => {
    expect(resolvePath(null, "a.txt")).toBe("a.txt");
  });
});

describe("parseOsc7", () => {
  it("returns a POSIX or msys path as reported", () => {
    expect(parseOsc7("file://host/home/x/proj")).toBe("/home/x/proj");
    expect(parseOsc7("file://host/c/Users/x")).toBe("/c/Users/x");
  });

  it("turns a native Windows path (PowerShell, cmd) back into native form", () => {
    expect(parseOsc7("file://PC/C:/Users/x/a%20b")).toBe("C:\\Users\\x\\a b");
    expect(parseOsc7("file://localhost/C:\\Users\\x")).toBe("C:\\Users\\x");
    expect(parseOsc7("file://localhost/D:")).toBe("D:\\");
  });

  it("rejects anything that is not a file URL", () => {
    expect(parseOsc7("http://x/y")).toBeNull();
    expect(parseOsc7("file://nohostpath")).toBeNull();
  });

  it("keeps a path with a literal percent sign", () => {
    expect(parseOsc7("file://h/C:/100%/x")).toBe("C:\\100%\\x");
  });
});
