import { describe, expect, it } from "vitest";
import {
  changeBetween,
  decodeText,
  detectIndent,
  encodeText,
  tabLabels,
} from "./model";

/** Apply a change, the way the buffer would. */
function apply(a: string, b: string): string {
  const change = changeBetween(a, b);
  if (!change) return a;
  return a.slice(0, change.from) + change.insert + a.slice(change.to);
}

describe("decodeText / encodeText", () => {
  it("round-trips the file's line endings and BOM", () => {
    for (const raw of ["a\nb\n", "a\r\nb\r\n", "\uFEFFa\r\nb", "", "no newline"]) {
      const { text, eol, bom } = decodeText(raw);
      expect(encodeText(text, eol, bom)).toBe(raw);
    }
  });

  it("gives the buffer plain \\n and no BOM", () => {
    expect(decodeText("\uFEFFa\r\nb\rc\n").text).toBe("a\nb\nc\n");
  });

  it("keeps a CRLF file CRLF across an edit", () => {
    const { text, eol, bom } = decodeText("one\r\ntwo\r\n");
    expect(encodeText(`${text}three\n`, eol, bom)).toBe("one\r\ntwo\r\nthree\r\n");
  });

  it("settles a mixed file on its majority ending", () => {
    expect(decodeText("a\r\nb\r\nc\n").eol).toBe("\r\n");
    expect(decodeText("a\nb\nc\r\n").eol).toBe("\n");
    expect(decodeText("a\r\nb\n").eol).toBe("\n");
  });
});

describe("detectIndent", () => {
  it("finds tabs", () => {
    expect(detectIndent("func main() {\n\tfoo()\n\tbar()\n}\n")).toBe("\t");
  });

  it("finds the space step, not the deepest indent", () => {
    expect(detectIndent("fn a() {\n    if x {\n        y();\n    }\n}\n")).toBe("    ");
    expect(detectIndent("a:\n  b:\n    c: 1\n  d: 2\n")).toBe("  ");
  });

  it("ignores blank lines and block-comment alignment", () => {
    expect(detectIndent("/**\n * doc\n * doc\n */\nfn a() {\n\n    b();\n}\n")).toBe("    ");
  });

  it("falls back when the file has no indentation", () => {
    expect(detectIndent("")).toBe("  ");
    expect(detectIndent("a\nb\n")).toBe("  ");
  });
});

describe("changeBetween", () => {
  it("is null for equal texts", () => {
    expect(changeBetween("same", "same")).toBeNull();
  });

  it("covers only what differs", () => {
    expect(changeBetween("hello world", "hello brave world")).toEqual({
      from: 6,
      to: 6,
      insert: "brave ",
    });
  });

  it("produces the target in every shape of change", () => {
    const cases = [
      ["abc", "abXc"],
      ["abc", "ac"],
      ["", "new"],
      ["old", ""],
      ["aaa", "aaaa"],
      ["aaaa", "aa"],
      ["abcabc", "abc"],
      ["x\ny\nz", "x\nz"],
    ];
    for (const [a, b] of cases) expect(apply(a, b)).toBe(b);
  });
});

describe("tabLabels", () => {
  it("uses the file name while it is unambiguous", () => {
    expect(tabLabels(["/p/src/main.ts", "C:\\p\\README.md"])).toEqual([
      "main.ts",
      "README.md",
    ]);
  });

  it("adds the parent directory to names that collide", () => {
    expect(
      tabLabels(["/p/editor/model.ts", "C:\\p\\sidebar\\model.ts", "/p/other.ts"]),
    ).toEqual(["editor/model.ts", "sidebar/model.ts", "other.ts"]);
  });
});
