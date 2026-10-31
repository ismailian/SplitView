import { afterEach, describe, it, expect, vi } from "vitest";
import {
  clampDirSegments,
  defaultSettings,
  loadSettings,
  MAX_DIR_SEGMENTS,
} from "./settings";

describe("remote resize setting", () => {
  afterEach(() => vi.unstubAllGlobals());

  const storing = (remote: unknown) =>
    vi.stubGlobal("localStorage", {
      getItem: () => JSON.stringify({ remote }),
    });

  it("is on by default, including for a store that predates it", () => {
    expect(defaultSettings().remote.allowResize).toBe(true);
    storing({ port: 9000 });
    expect(loadSettings().remote.allowResize).toBe(true);
  });

  it("keeps a saved choice to turn it off", () => {
    storing({ allowResize: false });
    expect(loadSettings().remote.allowResize).toBe(false);
  });

  it("reads a corrupt value as off", () => {
    storing({ allowResize: "yes" });
    expect(loadSettings().remote.allowResize).toBe(false);
  });
});

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
