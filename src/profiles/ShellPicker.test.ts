import { describe, it, expect } from "vitest";
import { resolveDefault } from "./ShellPicker";
import type { ShellProfile } from "../ipc/commands";

const bash: ShellProfile = {
  id: "git-bash",
  name: "Git Bash",
  program: "C:\\Program Files\\Git\\bin\\bash.exe",
  family: "posix",
};
const ps: ShellProfile = {
  id: "powershell",
  name: "Windows PowerShell",
  program: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
  family: "powershell",
};

describe("resolveDefault", () => {
  it("asks when nothing has been chosen yet", () => {
    expect(resolveDefault([bash, ps], null)).toBeNull();
  });

  it("uses the saved choice while it is installed", () => {
    expect(resolveDefault([bash, ps], "powershell")).toBe(ps);
  });

  it("asks again when the saved shell is gone", () => {
    expect(resolveDefault([ps], "git-bash")).toBeNull();
  });
});
