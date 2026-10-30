/**
 * Tests for the QR encoder, in two layers.
 *
 * 1. **A decoder.** `decodeSymbol` below reads a symbol back the way a scanner
 *    does — unmask, walk the zigzag, de-interleave, check the Reed-Solomon
 *    syndromes, parse the header — and asserts it yields the original text. The
 *    syndrome check is the important half: evaluating the received polynomial
 *    at the roots of its generator and requiring zero is an independent
 *    mathematical statement about the error-correction codewords, so a bug in
 *    `rsEncode` cannot slip past it.
 *
 * 2. **Golden symbols** in `qr.fixtures.json`, which pin the exact module layout
 *    against regressions. Their provenance is in the file's own `_comment`:
 *    every one was verified to decode with OpenCV's `cv2.QRCodeDetector`, and
 *    four of the six are identical module-for-module to `python-qrcode`. The
 *    other two differ from it only in which of the eight masks was picked —
 *    python-qrcode scores candidates with placeholder format bits, this encoder
 *    scores the finished symbol as ISO/IEC 18004 §7.8.3 describes — and mask
 *    choice is a readability heuristic, not correctness.
 *
 * This matters more than the line count suggests: the QR is how the certificate
 * fingerprint reaches the phone, and pinning the wrong one is the difference
 * between an encrypted channel and a convincing fake.
 */

import { describe, expect, it } from "vitest";
import fixtures from "./qr.fixtures.json";
import { MAX_VERSION, QrTooLongError, qrMatrix, qrSvg } from "./qr";

interface Fixture {
  text: string;
  version: number;
  size: number;
  matrix: string[];
  /** True when python-qrcode produces this symbol module-for-module. */
  matchesPythonQrcode: boolean;
}

// `_comment` records the fixtures' provenance; it is not a test case.
const { _comment, ...cases } = fixtures as unknown as Record<string, Fixture> & {
  _comment: string;
};
void _comment;

function render(matrix: boolean[][]): string[] {
  return matrix.map((row) => row.map((dark) => (dark ? "1" : "0")).join(""));
}

// --- an independent reader ------------------------------------------------

const EXP = new Uint8Array(512);
const LOG = new Uint8Array(256);
{
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
}
const mul = (a: number, b: number): number => (a === 0 || b === 0 ? 0 : EXP[LOG[a] + LOG[b]]);

/** Level-M block structure, from the standard's Table 13. */
const ECC_M: Record<number, [number, number, number, number, number]> = {
  1: [10, 1, 16, 0, 0],
  2: [16, 1, 28, 0, 0],
  3: [26, 1, 44, 0, 0],
  4: [18, 2, 32, 0, 0],
  5: [24, 2, 43, 0, 0],
  6: [16, 4, 27, 0, 0],
  7: [18, 4, 31, 0, 0],
  8: [22, 2, 38, 2, 39],
  9: [22, 3, 36, 2, 37],
  10: [26, 4, 43, 1, 44],
};

const ALIGNMENT: Record<number, number[]> = {
  1: [],
  2: [6, 18],
  3: [6, 22],
  4: [6, 26],
  5: [6, 30],
  6: [6, 34],
  7: [6, 22, 38],
  8: [6, 24, 42],
  9: [6, 26, 46],
  10: [6, 28, 50],
};

function maskBit(mask: number, x: number, y: number): boolean {
  const rules = [
    (x + y) % 2 === 0,
    y % 2 === 0,
    x % 3 === 0,
    (x + y) % 3 === 0,
    (Math.floor(y / 2) + Math.floor(x / 3)) % 2 === 0,
    ((x * y) % 2) + ((x * y) % 3) === 0,
    (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
    (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
  ];
  return rules[mask];
}

/** Rebuild the map of modules that carry no data, from the version alone. */
function functionModules(size: number, version: number): boolean[][] {
  const reserved = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
  const mark = (x: number, y: number): void => {
    if (x >= 0 && y >= 0 && x < size && y < size) reserved[y][x] = true;
  };
  for (const [ox, oy] of [
    [0, 0],
    [size - 7, 0],
    [0, size - 7],
  ]) {
    for (let dy = -1; dy <= 7; dy++) for (let dx = -1; dx <= 7; dx++) mark(ox + dx, oy + dy);
  }
  for (let i = 8; i < size - 8; i++) {
    mark(i, 6);
    mark(6, i);
  }
  for (const cy of ALIGNMENT[version]) {
    for (const cx of ALIGNMENT[version]) {
      const nearFinder =
        (cx <= 8 && cy <= 8) || (cx <= 8 && cy >= size - 9) || (cx >= size - 9 && cy <= 8);
      if (nearFinder) continue;
      for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) mark(cx + dx, cy + dy);
    }
  }
  for (let i = 0; i < 9; i++) {
    mark(i, 8);
    mark(8, i);
  }
  for (let i = 0; i < 8; i++) {
    mark(size - 1 - i, 8);
    mark(8, size - 1 - i);
  }
  mark(8, size - 8);
  if (version >= 7) {
    for (let i = 0; i < 18; i++) {
      const a = Math.floor(i / 3);
      const b = (i % 3) + size - 11;
      mark(a, b);
      mark(b, a);
    }
  }
  return reserved;
}

/** Read the 15-bit format information back out and recover the mask. */
function readFormat(matrix: boolean[][]): { eccLevel: number; mask: number } {
  const bit = (x: number, y: number): number => (matrix[y][x] ? 1 : 0);
  let bits = 0;
  for (let i = 0; i <= 5; i++) bits |= bit(8, i) << i;
  bits |= bit(8, 7) << 6;
  bits |= bit(8, 8) << 7;
  bits |= bit(7, 8) << 8;
  for (let i = 9; i < 15; i++) bits |= bit(14 - i, 8) << i;
  const data = (bits ^ 0x5412) >>> 10;
  return { eccLevel: data >> 3, mask: data & 7 };
}

/**
 * Evaluate the received block at each root of the generator polynomial; a valid
 * codeword gives zero at every one. QR's generator has roots α⁰…α^(t-1), so
 * that is where the block is sampled.
 */
function syndromesAreZero(block: number[], ecc: number): boolean {
  for (let i = 0; i < ecc; i++) {
    let value = 0;
    for (const coefficient of block) value = mul(value, EXP[i]) ^ coefficient;
    if (value !== 0) return false;
  }
  return true;
}

interface Decoded {
  text: string;
  version: number;
  mask: number;
  eccLevel: number;
  eccValid: boolean;
}

/** Read a symbol the way a scanner does. Throws if it is not well-formed. */
function decodeSymbol(matrix: boolean[][]): Decoded {
  const size = matrix.length;
  const version = (size - 17) / 4;
  const { mask, eccLevel } = readFormat(matrix);
  const reserved = functionModules(size, version);

  // Walk the zigzag, undoing the mask as we go.
  const bits: number[] = [];
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vert = 0; vert < size; vert++) {
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const upward = ((right + 1) & 2) === 0;
        const y = upward ? size - 1 - vert : vert;
        if (reserved[y][x]) continue;
        bits.push((matrix[y][x] ? 1 : 0) ^ (maskBit(mask, x, y) ? 1 : 0));
      }
    }
  }

  const codewords: number[] = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) {
    let byte = 0;
    for (let b = 0; b < 8; b++) byte = (byte << 1) | bits[i + b];
    codewords.push(byte);
  }

  // Undo the interleaving back into per-block data and ECC.
  const [ecc, blocks1, data1, blocks2, data2] = ECC_M[version];
  const sizes = [
    ...new Array<number>(blocks1).fill(data1),
    ...new Array<number>(blocks2).fill(data2),
  ];
  const dataBlocks: number[][] = sizes.map(() => []);
  let cursor = 0;
  const longest = Math.max(...sizes);
  for (let i = 0; i < longest; i++) {
    sizes.forEach((length, b) => {
      if (i < length) dataBlocks[b].push(codewords[cursor++]);
    });
  }
  const eccBlocks: number[][] = sizes.map(() => []);
  for (let i = 0; i < ecc; i++) {
    for (let b = 0; b < sizes.length; b++) eccBlocks[b].push(codewords[cursor++]);
  }

  const eccValid = dataBlocks.every((block, b) =>
    syndromesAreZero([...block, ...eccBlocks[b]], ecc),
  );

  // Parse the header out of the concatenated data blocks.
  const payload = dataBlocks.flat();
  const stream = payload.map((b) => b.toString(2).padStart(8, "0")).join("");
  const mode = parseInt(stream.slice(0, 4), 2);
  if (mode !== 0b0100) throw new Error(`expected byte mode, got ${mode}`);
  const countBits = version >= 10 ? 16 : 8;
  const count = parseInt(stream.slice(4, 4 + countBits), 2);
  const start = 4 + countBits;
  const bytes = new Uint8Array(count);
  for (let i = 0; i < count; i++) {
    bytes[i] = parseInt(stream.slice(start + i * 8, start + i * 8 + 8), 2);
  }

  return {
    text: new TextDecoder().decode(bytes),
    version,
    mask,
    eccLevel,
    eccValid,
  };
}

// --- tests ----------------------------------------------------------------

describe("qrMatrix", () => {
  for (const [name, fixture] of Object.entries(cases)) {
    it(`round-trips "${name}" through an independent reader (version ${fixture.version})`, () => {
      const decoded = decodeSymbol(qrMatrix(fixture.text));
      expect(decoded.text).toBe(fixture.text);
      expect(decoded.version).toBe(fixture.version);
      // Level M is 0b00 in the format information.
      expect(decoded.eccLevel).toBe(0b00);
      expect(decoded.eccValid).toBe(true);
      expect(decoded.mask).toBeGreaterThanOrEqual(0);
      expect(decoded.mask).toBeLessThan(8);
    });

    it(`reproduces the golden symbol for "${name}"`, () => {
      const matrix = qrMatrix(fixture.text);
      expect(matrix.length).toBe(fixture.size);
      expect(render(matrix)).toEqual(fixture.matrix);
    });
  }

  it("covers the shapes the pairing payload actually takes", () => {
    // The fixtures are only worth something if they exercise the encoder's
    // branches: one block, two block groups, and the version-information block
    // that appears from version 7.
    const versions = Object.values(cases).map((f) => f.version);
    expect(Math.min(...versions)).toBeLessThanOrEqual(2);
    expect(Math.max(...versions)).toBeGreaterThanOrEqual(7);
    // And most of them agree with a third-party encoder module-for-module.
    const agreeing = Object.values(cases).filter((f) => f.matchesPythonQrcode);
    expect(agreeing.length).toBeGreaterThanOrEqual(4);
  });

  it("corrupting a module breaks the error-correction check", () => {
    // Proves the syndrome test above is actually testing something.
    const matrix = qrMatrix("SPLITVIEW");
    matrix[12][12] = !matrix[12][12];
    expect(decodeSymbol(matrix).eccValid).toBe(false);
  });

  it("encodes multi-byte characters as UTF-8 bytes, not code points", () => {
    // A hostname can contain anything; the payload is bytes.
    expect(decodeSymbol(qrMatrix("café")).text).toBe("café");
  });

  it("refuses a payload that does not fit rather than emitting a broken symbol", () => {
    expect(() => qrMatrix("x".repeat(214))).toThrow(QrTooLongError);
    // The documented ceiling: version 10, level M, byte mode.
    expect(() => qrMatrix("x".repeat(213))).not.toThrow();
    expect(MAX_VERSION).toBe(10);
  });

  it("grows one version at a time as the payload grows", () => {
    const versions = new Set<number>();
    for (let length = 1; length <= 213; length += 7) {
      versions.add((qrMatrix("x".repeat(length)).length - 17) / 4);
    }
    // Every version from 1 to 10 is reachable, and each symbol is square.
    expect([...versions].sort((a, b) => a - b)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
  });

  it("is square, and every symbol size is 4n+17", () => {
    for (const fixture of Object.values(cases)) {
      const matrix = qrMatrix(fixture.text);
      expect(matrix.every((row) => row.length === matrix.length)).toBe(true);
      expect(matrix.length).toBe(fixture.version * 4 + 17);
    }
  });
});

describe("qrSvg", () => {
  it("draws every dark module inside a quiet zone", () => {
    const matrix = qrMatrix("SPLITVIEW");
    const svg = qrSvg(matrix);
    const dark = matrix.flat().filter(Boolean).length;
    // One `M…z` sub-path per dark module.
    expect(svg.match(/M\d+ \d+h1v1h-1z/g)?.length).toBe(dark);
    // 21 modules + 4 of quiet zone on each side.
    expect(svg).toContain('viewBox="0 0 29 29"');
  });

  it("honours a custom quiet zone", () => {
    const svg = qrSvg(qrMatrix("SPLITVIEW"), { quietZone: 0 });
    expect(svg).toContain('viewBox="0 0 21 21"');
  });

  it("is high contrast regardless of the app theme", () => {
    // Scanners need black on white; the surrounding UI can be whatever it likes.
    const svg = qrSvg(qrMatrix("SPLITVIEW"));
    expect(svg).toContain('fill="#ffffff"');
    expect(svg).toContain('fill="#000000"');
  });
});
