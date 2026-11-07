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

/**
 * Parse an OSC 7 payload (`file://host/path`) into the bare path. The host is
 * discarded; the path is percent-decoded best-effort. Returns null if it isn't
 * a `file://` URL.
 *
 * On Windows the path arrives in one of two forms, depending on the shell:
 * bash reports its msys `$PWD` (`/c/Users/x`), which the backend converts when
 * spawning the next pane; PowerShell and cmd report a native path after the
 * host's slash (`/C:/Users/x`, `/C:\Users\x`), which is returned as the native
 * `C:\Users\x` so the sidebar and the next split can use it directly.
 */
export function parseOsc7(data: string): string | null {
  if (!data.startsWith("file://")) return null;
  const rest = data.slice("file://".length);
  const slash = rest.indexOf("/");
  if (slash < 0) return null;
  let path = rest.slice(slash);
  try {
    path = decodeURIComponent(path);
  } catch {
    // Malformed escape (a literal `%` in a directory name); use the raw path.
  }
  if (/^\/[A-Za-z]:([\\/]|$)/.test(path)) {
    const native = path.slice(1).replace(/\//g, "\\");
    return native.length === 2 ? `${native}\\` : native;
  }
  return path;
}

/** The last segment of a path (either separator), or the path itself for a root. */
export function baseName(p: string): string {
  const parts = p.split(/[\\/]+/).filter((s) => s !== "");
  return parts.length > 0 ? parts[parts.length - 1] : p;
}

/**
 * Resolve a path typed at the shell against that shell's directory. Absolute
 * paths (POSIX, msys or native Windows) pass through; `cwd` is in whatever form
 * the shell reported, which the backend accepts.
 */
export function resolvePath(cwd: string | null, p: string): string {
  if (cwd === null || p.startsWith("/") || isWindowsPath(p)) return p;
  return `${cwd.replace(/[\\/]+$/, "")}/${p}`;
}

/**
 * How the injected prompt renders the working directory, mirrored here so the
 * Settings preview shows the real thing.
 *
 * `segments === 0` reproduces the shell's `__sv_compact`: the full path, with
 * everything between the first two segments and the last one collapsed to `...`
 * once the path is deep enough. `segments > 0` keeps only that many trailing
 * segments (`2` of `~/projects/project-x/mobile-app` → `project-x/mobile-app`),
 * falling back to the whole path when it has no more segments than asked for.
 *
 * Keep in sync with `__sv_compact` / `__sv_tail_segs` in `src-tauri/src/shell.rs`
 * — that is what actually draws the prompt; this is only the preview.
 */
export function promptDir(path: string, segments: number): string {
  const parts = path.split("/");
  if (segments > 0) {
    const named = parts.filter((s) => s !== "");
    return named.length > segments ? named.slice(-segments).join("/") : path;
  }
  // Fewer than three separators: short enough to show whole.
  if (parts.length - 1 < 3) return path;
  return `${parts[0]}/${parts[1]}/.../${parts[parts.length - 1]}`;
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
