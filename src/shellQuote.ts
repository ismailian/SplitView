/**
 * POSIX-quote a path for insertion at the shell prompt. Strings made only of
 * safe characters are inserted verbatim; anything with a space, backslash
 * (Windows paths) or shell metacharacter is wrapped in single quotes with any
 * embedded single quote escaped, so the path survives intact.
 *
 * Shared by the main and detached window controllers (the Win+O "insert path"
 * action, CLAUDE.md §1).
 */
export function shellQuote(path: string): string {
  if (path === "") return "''";
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(path)) return path;
  return "'" + path.replace(/'/g, "'\\''") + "'";
}
