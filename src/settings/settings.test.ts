import { describe, it, expect } from "vitest";
import { clampDirSegments, MAX_DIR_SEGMENTS } from "./settings";

describe("clampDirSegments", () => {
  it("keeps a sane segment count", () => {
    expect(clampDirSegments(1)).toBe(1);
    expect(clampDirSegments(2)).toBe(2);
    expect(clampDirSegments(MAX_DIR_SEGMENTS)).toBe(MAX_DIR_SEGMENTS);
  });

  it("collapses anything meaningless to 0 (= show the whole path)", () => {
    // The value ends up in the spawned shell's environment, so a stale or
    // hand-edited store must not smuggle junk through.
    expect(clampDirSegments(0)).toBe(0);
    expect(clampDirSegments(-3)).toBe(0);
    expect(clampDirSegments(undefined)).toBe(0);
    expect(clampDirSegments("nonsense")).toBe(0);
    expect(clampDirSegments(Number.NaN)).toBe(0);
  });

  it("caps a huge count and drops fractions", () => {
    expect(clampDirSegments(1000)).toBe(MAX_DIR_SEGMENTS);
    expect(clampDirSegments(2.9)).toBe(2);
  });
});
