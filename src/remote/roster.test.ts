import { describe, expect, it } from "vitest";
import { buildRoster, paneInfo, type RosterPaneView } from "./roster";

/** A stand-in for `Pane`, with only what the roster reads. */
function pane(overrides: Partial<RosterPaneView> & { preview?: string } = {}): RosterPaneView {
  const preview = overrides.preview ?? "";
  return {
    title: overrides.title ?? null,
    cwd: overrides.cwd ?? null,
    term: overrides.term ?? { cols: 80, rows: 24 },
    previewLine: () => preview,
  };
}

function mapOf(entries: [string, RosterPaneView][]): Map<string, RosterPaneView> {
  return new Map(entries);
}

describe("paneInfo", () => {
  it("carries everything only the webview knows", () => {
    const info = paneInfo(
      "pane-7",
      pane({
        title: "npm run dev",
        cwd: "/home/me/project-x",
        term: { cols: 120, rows: 34 },
        preview: "  ➜  Local:   http://localhost:5173/",
      }),
      false,
    );
    expect(info).toEqual({
      id: "pane-7",
      title: "npm run dev",
      cwd: "/home/me/project-x",
      cols: 120,
      rows: 34,
      preview: "  ➜  Local:   http://localhost:5173/",
      backgrounded: false,
    });
  });

  it("reports a pane with no title or cwd as null rather than a placeholder", () => {
    // The client decides how to label an unnamed tab; the desktop does not
    // invent a name for it.
    const info = paneInfo("pane-1", pane(), false);
    expect(info.title).toBeNull();
    expect(info.cwd).toBeNull();
    expect(info.preview).toBe("");
  });

  it("flags a backgrounded pane", () => {
    expect(paneInfo("pane-2", pane(), true).backgrounded).toBe(true);
  });
});

describe("buildRoster", () => {
  it("puts panes in layout order, then backgrounded ones", () => {
    const live = mapOf([
      ["pane-1", pane({ title: "one" })],
      ["pane-2", pane({ title: "two" })],
    ]);
    const background = mapOf([["pane-9", pane({ title: "dev server" })]]);

    const roster = buildRoster(["pane-2", "pane-1"], live, background);
    expect(roster.map((p) => p.id)).toEqual(["pane-2", "pane-1", "pane-9"]);
    expect(roster.map((p) => p.backgrounded)).toEqual([false, false, true]);
  });

  it("keeps backgrounded panes: they are still live terminals", () => {
    // The whole point of checking on a laptop from a phone is the dev server
    // you pushed out of the way.
    const background = mapOf([["pane-9", pane({ preview: "listening on :5173" })]]);
    const roster = buildRoster([], new Map(), background);
    expect(roster).toHaveLength(1);
    expect(roster[0].preview).toBe("listening on :5173");
  });

  it("skips a pane the layout still names but the window no longer has", () => {
    // A pane can close between the layout change and the publish; advertising
    // it would give the client a tab it can never subscribe to.
    const live = mapOf([["pane-1", pane()]]);
    const roster = buildRoster(["pane-1", "pane-gone"], live, new Map());
    expect(roster.map((p) => p.id)).toEqual(["pane-1"]);
  });

  it("never lists a pane twice", () => {
    const shared = pane();
    const live = mapOf([["pane-1", shared]]);
    const background = mapOf([["pane-1", shared]]);
    const roster = buildRoster(["pane-1"], live, background);
    expect(roster.map((p) => p.id)).toEqual(["pane-1"]);
    // The layout entry wins: that is where the pane actually is.
    expect(roster[0].backgrounded).toBe(false);
  });

  it("is empty when the window has no panes", () => {
    expect(buildRoster([], new Map(), new Map())).toEqual([]);
  });
});
