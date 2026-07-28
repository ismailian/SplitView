/**
 * Path helpers shared by the file sidebar (CLAUDE.md §1).
 *
 * The backend hands us **native** paths (`C:\Users\john\proj` on Windows,
 * `/home/john/proj` elsewhere) but SplitView always talks to a POSIX shell
 * (CLAUDE.md §18), which on Windows means an msys shell that wants
 * `/c/Users/john/proj`. These functions are pure string transforms so they are
 * testable without a filesystem.
 */

/** True for a native Windows path (`C:\…` or `C:/…`). */
function isWindowsPath(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p);
}

/**
 * Convert a native path into the form the spawned shell understands: on Windows
 * `C:\Users\john` becomes the msys `/c/Users/john`, and stray backslashes become
 * forward slashes. POSIX paths pass through untouched.
 */
export function toShellPath(p: string): string {
  if (isWindowsPath(p)) {
    const drive = p[0].toLowerCase();
    const rest = p.slice(2).replace(/\\/g, "/").replace(/^\/+/, "");
    return `/${drive}/${rest}`;
  }
  return p.replace(/\\/g, "/");
}

/** The last segment of a path (either separator), or the path itself for a root. */
export function baseName(p: string): string {
  const parts = p.split(/[\\/]+/).filter((s) => s !== "");
  return parts.length > 0 ? parts[parts.length - 1] : p;
}

/**
 * Shorten a path for a narrow header: collapse `$HOME`-less middles to
 * `first/…/last-two` once it exceeds `max` characters. Purely cosmetic — the
 * real path is always what gets sent to the shell.
 */
export function compactPath(p: string, max = 32): string {
  if (p.length <= max) return p;
  const sep = p.includes("\\") && !p.includes("/") ? "\\" : "/";
  const parts = p.split(/[\\/]+/).filter((s) => s !== "");
  if (parts.length <= 2) return p;
  const head = parts[0];
  const tail = parts.slice(-2).join(sep);
  const short = `${head}${sep}…${sep}${tail}`;
  // Still too long (one very long segment): keep the tail, which is what the
  // user is actually looking at.
  return short.length <= max ? short : `…${sep}${parts[parts.length - 1]}`;
}
