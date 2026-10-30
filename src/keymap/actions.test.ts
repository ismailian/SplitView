import { describe, expect, it } from "vitest";
import { ACTION_INFO, actionHandlers, dispatchAction, type AppActions } from "./actions";
import { DEFAULT_KEYMAP } from "./keymap";

/** Records which `AppActions` method an action name ends up calling. */
function recorder(): { calls: string[]; app: AppActions } {
  const calls: string[] = [];
  const app = new Proxy({} as AppActions, {
    get:
      (_target, property: string) =>
      (...args: unknown[]): void => {
        calls.push(args.length ? `${property}(${args.join(",")})` : property);
      },
  });
  return { calls, app };
}

describe("action dispatch", () => {
  it("every default binding names an action that exists", () => {
    const bound = [
      ...Object.values(DEFAULT_KEYMAP.prefixBindings),
      ...Object.values(DEFAULT_KEYMAP.globalBindings ?? {}),
    ];
    for (const action of bound) {
      expect(actionHandlers, `no handler for "${action}"`).toHaveProperty(action);
    }
  });

  it("every action a key can reach is described for the Settings reference", () => {
    const bound = new Set([
      ...Object.values(DEFAULT_KEYMAP.prefixBindings),
      ...Object.values(DEFAULT_KEYMAP.globalBindings ?? {}),
    ]);
    for (const action of bound) {
      expect(ACTION_INFO, `no label for "${action}"`).toHaveProperty(action);
    }
  });

  it("ignores an unknown action name instead of throwing", () => {
    const { calls, app } = recorder();
    dispatchAction("not_a_real_action", app);
    expect(calls).toEqual([]);
  });

  it("closing a pane from the keyboard goes through the confirming path", () => {
    const { calls, app } = recorder();
    dispatchAction("close_pane", app);
    expect(calls).toEqual(["closePane"]);
  });

  /**
   * The security property behind remote control: a remote client drives the
   * same actions the keymap drives, so it cannot do anything a keyboard user
   * cannot (`docs/remote-control-desktop.md` §5.4). `closePaneConfirmed` exists
   * only so a remote close does not pop a modal on an unattended laptop — if it
   * ever became reachable from a key, that key would close a pane with no
   * confirmation.
   */
  it("no keybinding can reach closePaneConfirmed", () => {
    const { calls, app } = recorder();
    for (const action of Object.keys(actionHandlers)) dispatchAction(action, app);
    expect(calls.some((call) => call.startsWith("closePaneConfirmed"))).toBe(false);
  });

  it("no action handler exists for a remote-only entry point", () => {
    // Belt and braces: the name must not appear in the dispatch table at all.
    expect(Object.keys(actionHandlers)).not.toContain("close_pane_confirmed");
    expect(Object.values(actionHandlers)).toHaveLength(
      Object.keys(actionHandlers).length,
    );
  });
});
