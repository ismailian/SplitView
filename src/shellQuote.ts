/**
 * Quote a path for insertion at a shell prompt, in the syntax of the shell that
 * will read it (CLAUDE.md §1). The sidebar, the file picker (Win+O) and `cd`
 * from the sidebar all type paths into a terminal; which shell is running there
 * decides both the form of the path and how it is quoted.
 *
 * Shared by the main and detached window controllers.
 */

import type { ShellFamily } from "./ipc/commands";
import { toShellPath } from "./paths";

/**
 * POSIX-quote a string. Strings made only of safe characters are inserted
 * verbatim; anything with a space, backslash (Windows paths) or shell
 * metacharacter is wrapped in single quotes with any embedded single quote
 * escaped, so the path survives intact.
 */
export function shellQuote(path: string): string {
  if (path === "") return "''";
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(path)) return path;
  return "'" + path.replace(/'/g, "'\\''") + "'";
}

/**
 * A native path (`C:\Users\x y`, `/home/x`), ready to type into a shell of
 * `family`:
 *   - POSIX: converted to the shell's form (`/c/Users/x y` under msys), then
 *     single-quoted if needed.
 *   - PowerShell: native, single-quoted if needed (`'` doubles inside).
 *   - cmd: native, double-quoted if needed (a `"` cannot occur in a Windows
 *     path, so there is nothing to escape).
 */
export function quotePathFor(family: ShellFamily, nativePath: string): string {
  switch (family) {
    case "posix":
      return shellQuote(toShellPath(nativePath));
    case "powershell":
      if (/^[A-Za-z0-9_:.\\/-]+$/.test(nativePath)) return nativePath;
      return "'" + nativePath.replace(/'/g, "''") + "'";
    case "cmd":
      if (/^[A-Za-z0-9_:.\\/-]+$/.test(nativePath)) return nativePath;
      return `"${nativePath}"`;
  }
}

/**
 * The command line (with its Enter) that moves a `family` shell into the native
 * directory `dir`. cmd needs `/d` to switch drives as well as directories.
 */
export function cdCommandFor(family: ShellFamily, dir: string): string {
  const quoted = quotePathFor(family, dir);
  return family === "cmd" ? `cd /d ${quoted}\r` : `cd ${quoted}\r`;
}
