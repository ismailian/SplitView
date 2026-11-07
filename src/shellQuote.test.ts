import { describe, it, expect } from "vitest";
import { cdCommandFor, quotePathFor, shellQuote } from "./shellQuote";

describe("shellQuote", () => {
  it("leaves safe strings alone and single-quotes the rest", () => {
    expect(shellQuote("/home/x/proj")).toBe("/home/x/proj");
    expect(shellQuote("/home/x/my proj")).toBe("'/home/x/my proj'");
    expect(shellQuote("it's")).toBe("'it'\\''s'");
  });
});

describe("quotePathFor", () => {
  it("gives POSIX shells the msys form on Windows", () => {
    expect(quotePathFor("posix", "C:\\Users\\x\\a b")).toBe("'/c/Users/x/a b'");
    expect(quotePathFor("posix", "/home/x")).toBe("/home/x");
  });

  it("gives PowerShell a native path, single-quoted when needed", () => {
    expect(quotePathFor("powershell", "C:\\Users\\x")).toBe("C:\\Users\\x");
    expect(quotePathFor("powershell", "C:\\a b\\it's")).toBe("'C:\\a b\\it''s'");
    // `$` would expand inside double quotes; single quotes keep it literal.
    expect(quotePathFor("powershell", "C:\\$x")).toBe("'C:\\$x'");
  });

  it("gives cmd a native path, double-quoted when needed", () => {
    expect(quotePathFor("cmd", "C:\\Users\\x")).toBe("C:\\Users\\x");
    expect(quotePathFor("cmd", "C:\\Program Files")).toBe('"C:\\Program Files"');
  });
});

describe("cdCommandFor", () => {
  it("uses cd /d in cmd so the drive changes too", () => {
    expect(cdCommandFor("cmd", "D:\\work")).toBe("cd /d D:\\work\r");
    expect(cdCommandFor("powershell", "D:\\work")).toBe("cd D:\\work\r");
    expect(cdCommandFor("posix", "D:\\work")).toBe("cd /d/work\r");
  });
});
