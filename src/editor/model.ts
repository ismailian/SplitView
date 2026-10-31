/**
 * The editor panel's pure logic: what has to survive a round trip through an
 * editor buffer, and what can be worked out from a file's text. No DOM and no
 * CodeMirror, so it is testable on its own (CLAUDE.md §17).
 */

import { baseName } from "../paths";

export type Eol = "\n" | "\r\n";

/** A file's text as the buffer holds it, plus what saving has to restore. */
export interface Decoded {
  /** BOM stripped, line endings normalized to `\n`. */
  text: string;
  eol: Eol;
  bom: boolean;
}

const BOM = "﻿";

/**
 * Prepare file contents for the buffer. The buffer only ever holds `\n`, so a
 * CRLF file has to remember that it is one: saving it as LF would turn a
 * one-line edit into a whole-file diff.
 */
export function decodeText(raw: string): Decoded {
  const bom = raw.startsWith(BOM);
  const body = bom ? raw.slice(BOM.length) : raw;
  return {
    text: body.replace(/\r\n?/g, "\n"),
    eol: detectEol(body),
    bom,
  };
}

/** The inverse of `decodeText`. A mixed file comes back with its majority ending. */
export function encodeText(text: string, eol: Eol, bom: boolean): string {
  const body = eol === "\n" ? text : text.replace(/\n/g, eol);
  return bom ? BOM + body : body;
}

/** The most common line ending; a tie, or a file with none, is `\n`. */
function detectEol(text: string): Eol {
  const crlf = text.match(/\r\n/g)?.length ?? 0;
  const lf = (text.match(/\n/g)?.length ?? 0) - crlf;
  return crlf > lf ? "\r\n" : "\n";
}

/** Lines sampled by `detectIndent`; enough to decide, cheap on a huge file. */
const INDENT_SAMPLE_LINES = 2000;
/** Space widths taken for an indent step. A step of 1 is the ` * ` of a block
 * comment or a continuation line, not the file's indentation. */
const INDENT_WIDTHS = [2, 3, 4, 8];
const DEFAULT_INDENT = "  ";

/**
 * The file's indent unit, so Tab and auto-indent match what is there: `\t` for a
 * tab-indented file, otherwise the most common step in leading spaces between
 * one line and the next.
 */
export function detectIndent(text: string): string {
  let tabs = 0;
  let spaced = 0;
  let previous = 0;
  const steps = new Map<number, number>();

  for (const line of text.split("\n", INDENT_SAMPLE_LINES)) {
    if (line.trim() === "") continue;
    if (line.startsWith("\t")) {
      tabs++;
      continue;
    }
    const width = line.length - line.trimStart().length;
    if (width > 0) spaced++;
    const step = Math.abs(width - previous);
    if (INDENT_WIDTHS.includes(step)) steps.set(step, (steps.get(step) ?? 0) + 1);
    previous = width;
  }

  if (tabs > spaced) return "\t";
  let best = 0;
  let bestCount = 0;
  for (const [step, count] of steps) {
    if (count > bestCount) {
      best = step;
      bestCount = count;
    }
  }
  return best > 0 ? " ".repeat(best) : DEFAULT_INDENT;
}

/** The replacement that turns `a` into `b`, or null if they are equal. */
export interface TextChange {
  from: number;
  to: number;
  insert: string;
}

/**
 * The smallest single replacement between two texts: everything between their
 * common prefix and suffix. Reloading through this leaves the cursor and scroll
 * position alone unless the change actually reaches them.
 */
export function changeBetween(a: string, b: string): TextChange | null {
  if (a === b) return null;
  const shortest = Math.min(a.length, b.length);
  let start = 0;
  while (start < shortest && a[start] === b[start]) start++;
  let end = 0;
  while (
    end < shortest - start &&
    a[a.length - 1 - end] === b[b.length - 1 - end]
  ) {
    end++;
  }
  return {
    from: start,
    to: a.length - end,
    insert: b.slice(start, b.length - end),
  };
}

/**
 * A label per open path: the file name, plus its parent directory where two tabs
 * would otherwise read the same (`editor/model.ts`, `sidebar/model.ts`).
 */
export function tabLabels(paths: readonly string[]): string[] {
  const names = paths.map(baseName);
  return paths.map((path, i) => {
    const name = names[i];
    if (names.filter((n) => n === name).length < 2) return name;
    const parts = path.split(/[\\/]+/).filter((s) => s !== "");
    return parts.slice(-2).join("/");
  });
}
