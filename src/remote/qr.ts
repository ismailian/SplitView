/**
 * A minimal QR Code encoder, for the pairing payload (CLAUDE.md §21,
 * `docs/remote-control-desktop.md` §7.2).
 *
 * The QR is how the certificate fingerprint reaches the phone: it is 43
 * characters of base64 that nobody is going to type correctly, and pinning the
 * wrong one is the difference between an encrypted channel and a fake one. So
 * the code has to be right, and it has to work with no network and no extra
 * dependency — which is why this is ~300 lines of ISO/IEC 18004 rather than an
 * npm package.
 *
 * Deliberately narrow: **byte mode, error-correction level M, versions 1–10**.
 * That tops out at 213 bytes, and the payload is ~110, so the encoder never
 * needs the 16-bit length header, the wide-version alignment tables, or the
 * other modes. Anything longer throws and the caller falls back to showing the
 * payload as text (which the manual-entry pairing path uses anyway).
 *
 * Verified against `segno`'s reference output; see `qr.test.ts`.
 */

/** Error-correction level M: ~15% recovery, the usual choice for a screen. */
const ECC_LEVEL_BITS = 0b00;

/** Byte mode. */
const MODE_BYTE = 0b0100;

/** The highest version this encoder builds. */
export const MAX_VERSION = 10;

/**
 * Per version (index 1..10), for level M:
 * `[eccCodewordsPerBlock, group1Blocks, group1DataCodewords, group2Blocks, group2DataCodewords]`.
 */
const ECC_M: readonly (readonly number[])[] = [
  [], // versions are 1-based
  [10, 1, 16, 0, 0],
  [16, 1, 28, 0, 0],
  [26, 1, 44, 0, 0],
  [18, 2, 32, 0, 0],
  [24, 2, 43, 0, 0],
  [16, 4, 27, 0, 0],
  [18, 4, 31, 0, 0],
  [22, 2, 38, 2, 39],
  [22, 3, 36, 2, 37],
  [26, 4, 43, 1, 44],
];

/** Alignment-pattern centre coordinates per version (none for version 1). */
const ALIGNMENT: readonly (readonly number[])[] = [
  [],
  [],
  [6, 18],
  [6, 22],
  [6, 26],
  [6, 30],
  [6, 34],
  [6, 22, 38],
  [6, 24, 42],
  [6, 26, 46],
  [6, 28, 50],
];

/** Thrown when the payload does not fit the versions this encoder supports. */
export class QrTooLongError extends Error {
  constructor(bytes: number) {
    super(`payload of ${bytes} bytes exceeds QR version ${MAX_VERSION} (byte mode, level M)`);
    this.name = "QrTooLongError";
  }
}

/** Total data codewords available at a version (both block groups). */
function dataCodewords(version: number): number {
  const [, blocks1, data1, blocks2, data2] = ECC_M[version];
  return blocks1 * data1 + blocks2 * data2;
}

/** Smallest version that fits `length` bytes, or `null` if none does. */
function pickVersion(length: number): number | null {
  for (let version = 1; version <= MAX_VERSION; version++) {
    // 4 bits of mode + 8 bits of length; versions 1–9 use an 8-bit count, and
    // version 10 would use 16 — which is why the loop stops caring at 10 and
    // the capacity below is computed for the 8-bit case only up to 9.
    const headerBits = version >= 10 ? 4 + 16 : 4 + 8;
    const capacity = dataCodewords(version) - Math.ceil(headerBits / 8);
    if (length <= capacity) return version;
  }
  return null;
}

// --- GF(256) -------------------------------------------------------------
// Reed-Solomon over the field the QR standard specifies: primitive polynomial
// x^8 + x^4 + x^3 + x^2 + 1 (0x11d), generator 2.

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

function gfMul(a: number, b: number): number {
  if (a === 0 || b === 0) return 0;
  return EXP[LOG[a] + LOG[b]];
}

/** The generator polynomial of degree `degree`, coefficients high-order first. */
function rsGenerator(degree: number): Uint8Array {
  let poly = new Uint8Array([1]);
  for (let i = 0; i < degree; i++) {
    const next = new Uint8Array(poly.length + 1);
    for (let j = 0; j < poly.length; j++) {
      next[j] ^= poly[j];
      next[j + 1] ^= gfMul(poly[j], EXP[i]);
    }
    poly = next;
  }
  return poly;
}

/** The `degree` error-correction codewords for one block. */
function rsEncode(data: Uint8Array, degree: number): Uint8Array {
  const generator = rsGenerator(degree);
  const remainder = new Uint8Array(degree);
  for (const byte of data) {
    const factor = byte ^ remainder[0];
    remainder.copyWithin(0, 1);
    remainder[degree - 1] = 0;
    for (let i = 0; i < degree; i++) {
      remainder[i] ^= gfMul(generator[i + 1], factor);
    }
  }
  return remainder;
}

// --- bit stream ----------------------------------------------------------

class BitBuffer {
  private readonly bits: number[] = [];

  push(value: number, length: number): void {
    for (let i = length - 1; i >= 0; i--) this.bits.push((value >>> i) & 1);
  }

  get length(): number {
    return this.bits.length;
  }

  /** Pad to a whole number of codewords and return them. */
  toCodewords(total: number): Uint8Array {
    // Terminator: up to four zero bits, then pad to a byte boundary.
    const capacity = total * 8;
    const terminator = Math.min(4, capacity - this.bits.length);
    for (let i = 0; i < terminator; i++) this.bits.push(0);
    while (this.bits.length % 8 !== 0) this.bits.push(0);

    const out = new Uint8Array(total);
    for (let i = 0; i < this.bits.length; i += 8) {
      let byte = 0;
      for (let b = 0; b < 8; b++) byte = (byte << 1) | this.bits[i + b];
      out[i / 8] = byte;
    }
    // The standard's pad bytes, alternating, for whatever is left.
    const pad = [0xec, 0x11];
    for (let i = this.bits.length / 8, n = 0; i < total; i++, n++) {
      out[i] = pad[n % 2];
    }
    return out;
  }
}

/** Interleave the data and ECC blocks exactly as the standard prescribes. */
function buildCodewords(data: Uint8Array, version: number): Uint8Array {
  const [eccPerBlock, blocks1, data1, blocks2, data2] = ECC_M[version];
  const bits = new BitBuffer();
  bits.push(MODE_BYTE, 4);
  bits.push(data.length, version >= 10 ? 16 : 8);
  for (const byte of data) bits.push(byte, 8);

  const payload = bits.toCodewords(dataCodewords(version));

  const dataBlocks: Uint8Array[] = [];
  const eccBlocks: Uint8Array[] = [];
  let offset = 0;
  for (const [count, size] of [
    [blocks1, data1],
    [blocks2, data2],
  ]) {
    for (let i = 0; i < count; i++) {
      const block = payload.subarray(offset, offset + size);
      offset += size;
      dataBlocks.push(block);
      eccBlocks.push(rsEncode(block, eccPerBlock));
    }
  }

  const out: number[] = [];
  const longest = Math.max(...dataBlocks.map((b) => b.length));
  for (let i = 0; i < longest; i++) {
    for (const block of dataBlocks) if (i < block.length) out.push(block[i]);
  }
  for (let i = 0; i < eccPerBlock; i++) {
    for (const block of eccBlocks) out.push(block[i]);
  }
  return Uint8Array.from(out);
}

// --- matrix --------------------------------------------------------------

type Grid = Int8Array[]; // -1 = free, 0 = light, 1 = dark

function emptyGrid(size: number): Grid {
  return Array.from({ length: size }, () => new Int8Array(size).fill(-1));
}

function setFunction(grid: Grid, reserved: boolean[][], x: number, y: number, dark: boolean): void {
  grid[y][x] = dark ? 1 : 0;
  reserved[y][x] = true;
}

/** Finder patterns, separators, timing, alignment, and the dark module. */
function drawFunctionPatterns(grid: Grid, reserved: boolean[][], version: number): void {
  const size = grid.length;

  const finder = (ox: number, oy: number): void => {
    for (let dy = -1; dy <= 7; dy++) {
      for (let dx = -1; dx <= 7; dx++) {
        const x = ox + dx;
        const y = oy + dy;
        if (x < 0 || y < 0 || x >= size || y >= size) continue;
        const ring = Math.max(Math.abs(dx - 3), Math.abs(dy - 3));
        setFunction(grid, reserved, x, y, ring !== 2 && ring <= 3);
      }
    }
  };
  finder(0, 0);
  finder(size - 7, 0);
  finder(0, size - 7);

  // Timing patterns.
  for (let i = 8; i < size - 8; i++) {
    setFunction(grid, reserved, i, 6, i % 2 === 0);
    setFunction(grid, reserved, 6, i, i % 2 === 0);
  }

  // Alignment patterns, skipping the three finder corners.
  const centres = ALIGNMENT[version];
  for (const cy of centres) {
    for (const cx of centres) {
      const nearFinder =
        (cx <= 8 && cy <= 8) || (cx <= 8 && cy >= size - 9) || (cx >= size - 9 && cy <= 8);
      if (nearFinder) continue;
      for (let dy = -2; dy <= 2; dy++) {
        for (let dx = -2; dx <= 2; dx++) {
          const ring = Math.max(Math.abs(dx), Math.abs(dy));
          setFunction(grid, reserved, cx + dx, cy + dy, ring !== 1);
        }
      }
    }
  }

  // Reserve the format-information areas (written after masking).
  for (let i = 0; i < 9; i++) {
    if (!reserved[8][i]) setFunction(grid, reserved, i, 8, false);
    if (!reserved[i][8]) setFunction(grid, reserved, 8, i, false);
  }
  for (let i = 0; i < 8; i++) {
    setFunction(grid, reserved, size - 1 - i, 8, false);
    setFunction(grid, reserved, 8, size - 1 - i, false);
  }
  // The module that is always dark.
  setFunction(grid, reserved, 8, size - 8, true);

  if (version >= 7) drawVersionInfo(grid, reserved, version);
}

/** The 18-bit version information block, present from version 7 (BCH 18,6). */
function drawVersionInfo(grid: Grid, reserved: boolean[][], version: number): void {
  let remainder = version;
  for (let i = 0; i < 12; i++) {
    remainder = (remainder << 1) ^ ((remainder >>> 11) * 0x1f25);
  }
  const bits = ((version << 12) | remainder) >>> 0;
  const size = grid.length;

  for (let i = 0; i < 18; i++) {
    const dark = ((bits >>> i) & 1) === 1;
    const a = Math.floor(i / 3);
    const b = (i % 3) + size - 11;
    setFunction(grid, reserved, a, b, dark);
    setFunction(grid, reserved, b, a, dark);
  }
}

/** The 15-bit format information (BCH 15,5, masked with 0x5412). */
function drawFormatInfo(grid: Grid, mask: number): void {
  const data = (ECC_LEVEL_BITS << 3) | mask;
  let remainder = data;
  for (let i = 0; i < 10; i++) {
    remainder = (remainder << 1) ^ ((remainder >>> 9) * 0x537);
  }
  const bits = (((data << 10) | remainder) ^ 0x5412) >>> 0;
  const size = grid.length;

  const put = (x: number, y: number, i: number): void => {
    grid[y][x] = ((bits >>> i) & 1) as 0 | 1;
  };

  // Copy 1: down the left of the top-left finder, then along the top of it.
  for (let i = 0; i <= 5; i++) put(8, i, i);
  put(8, 7, 6);
  put(8, 8, 7);
  put(7, 8, 8);
  for (let i = 9; i < 15; i++) put(14 - i, 8, i);

  // Copy 2: along the top-right finder, then down the bottom-left one.
  for (let i = 0; i < 8; i++) put(size - 1 - i, 8, i);
  for (let i = 8; i < 15; i++) put(8, size - 15 + i, i);
}

/** Lay the codewords out in the standard two-column zigzag. */
function placeCodewords(grid: Grid, reserved: boolean[][], codewords: Uint8Array): void {
  const size = grid.length;
  let bit = 0;
  const total = codewords.length * 8;

  for (let right = size - 1; right >= 1; right -= 2) {
    // Column 6 is the vertical timing pattern; the pairs step around it.
    if (right === 6) right = 5;
    for (let vert = 0; vert < size; vert++) {
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const upward = ((right + 1) & 2) === 0;
        const y = upward ? size - 1 - vert : vert;
        if (reserved[y][x]) continue;
        // Modules past the last data bit stay light (the "remainder bits").
        const dark = bit < total && ((codewords[bit >>> 3] >>> (7 - (bit & 7))) & 1) === 1;
        grid[y][x] = dark ? 1 : 0;
        bit++;
      }
    }
  }
}

function maskBit(mask: number, x: number, y: number): boolean {
  switch (mask) {
    case 0:
      return (x + y) % 2 === 0;
    case 1:
      return y % 2 === 0;
    case 2:
      return x % 3 === 0;
    case 3:
      return (x + y) % 3 === 0;
    case 4:
      return (Math.floor(y / 2) + Math.floor(x / 3)) % 2 === 0;
    case 5:
      return ((x * y) % 2) + ((x * y) % 3) === 0;
    case 6:
      return (((x * y) % 2) + ((x * y) % 3)) % 2 === 0;
    default:
      return (((x + y) % 2) + ((x * y) % 3)) % 2 === 0;
  }
}

function applyMask(grid: Grid, reserved: boolean[][], mask: number): void {
  const size = grid.length;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (reserved[y][x]) continue;
      if (maskBit(mask, x, y)) grid[y][x] ^= 1;
    }
  }
}

/** The standard's four penalty rules; the lowest total wins. */
function penalty(grid: Grid): number {
  const size = grid.length;
  let score = 0;

  // Rule 1: runs of five or more same-coloured modules in a row or column.
  const runs = (get: (i: number, j: number) => number): void => {
    for (let i = 0; i < size; i++) {
      let run = 1;
      for (let j = 1; j < size; j++) {
        if (get(i, j) === get(i, j - 1)) {
          run++;
          if (run === 5) score += 3;
          else if (run > 5) score += 1;
        } else {
          run = 1;
        }
      }
    }
  };
  runs((y, x) => grid[y][x]);
  runs((x, y) => grid[y][x]);

  // Rule 2: 2x2 blocks of one colour.
  for (let y = 0; y < size - 1; y++) {
    for (let x = 0; x < size - 1; x++) {
      const v = grid[y][x];
      if (v === grid[y][x + 1] && v === grid[y + 1][x] && v === grid[y + 1][x + 1]) {
        score += 3;
      }
    }
  }

  // Rule 3: the finder-like 1:1:3:1:1 pattern with four light modules beside it.
  const pattern = [1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 0];
  const reverse = [0, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1];
  const matches = (get: (k: number) => number, start: number, want: number[]): boolean =>
    want.every((bit, k) => get(start + k) === bit);
  for (let i = 0; i < size; i++) {
    for (let j = 0; j + 11 <= size; j++) {
      const row = (k: number): number => grid[i][k];
      const col = (k: number): number => grid[k][i];
      if (matches(row, j, pattern) || matches(row, j, reverse)) score += 40;
      if (matches(col, j, pattern) || matches(col, j, reverse)) score += 40;
    }
  }

  // Rule 4: imbalance between dark and light.
  let dark = 0;
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) dark += grid[y][x];
  const percent = (dark * 100) / (size * size);
  score += Math.floor(Math.abs(percent - 50) / 5) * 10;

  return score;
}

/**
 * Encode `text` as a QR matrix: `true` is a dark module.
 *
 * Throws [`QrTooLongError`] if it does not fit; the caller shows the payload as
 * text instead (manual entry is a supported, if second-class, pairing path).
 */
export function qrMatrix(text: string): boolean[][] {
  const data = new TextEncoder().encode(text);
  const version = pickVersion(data.length);
  if (version === null) throw new QrTooLongError(data.length);

  const codewords = buildCodewords(data, version);
  const size = version * 4 + 17;

  let best: Grid | null = null;
  let bestScore = Infinity;
  for (let mask = 0; mask < 8; mask++) {
    const grid = emptyGrid(size);
    const reserved = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
    drawFunctionPatterns(grid, reserved, version);
    placeCodewords(grid, reserved, codewords);
    applyMask(grid, reserved, mask);
    drawFormatInfo(grid, mask);

    const score = penalty(grid);
    if (score < bestScore) {
      bestScore = score;
      best = grid;
    }
  }

  // `best` is always set: the loop runs eight times and any score beats Infinity.
  const chosen = best ?? emptyGrid(size);
  return chosen.map((row) => Array.from(row, (v) => v === 1));
}

/**
 * Render a QR matrix as an inline SVG string.
 *
 * SVG rather than a canvas so it stays crisp at any size and needs no device
 * pixel-ratio handling, and it is drawn in the app's own colours — the scanner
 * only cares about contrast.
 */
export function qrSvg(matrix: boolean[][], options: { quietZone?: number } = {}): string {
  const quiet = options.quietZone ?? 4;
  const size = matrix.length + quiet * 2;
  const path: string[] = [];
  matrix.forEach((row, y) => {
    row.forEach((dark, x) => {
      if (dark) path.push(`M${x + quiet} ${y + quiet}h1v1h-1z`);
    });
  });
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}"`,
    ` shape-rendering="crispEdges" role="img" aria-label="Pairing QR code">`,
    `<rect width="${size}" height="${size}" fill="#ffffff"/>`,
    `<path fill="#000000" d="${path.join("")}"/>`,
    `</svg>`,
  ].join("");
}
