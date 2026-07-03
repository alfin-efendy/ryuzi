import { describe, expect, test } from "bun:test";
import { goBackHistory, goForwardHistory, navigateHistory, type NavHistory, type View } from "./store-nav";

const home: View = { kind: "home" };
const providers: View = { kind: "providers" };
const detail: View = { kind: "providerDetail", id: "anthropic" };

const start: NavHistory = { back: [], current: home, forward: [] };

describe("nav history", () => {
  test("navigate pushes current onto back and clears forward", () => {
    const h1 = navigateHistory(start, providers);
    expect(h1.current).toEqual(providers);
    expect(h1.back).toEqual([home]);
    const h2 = navigateHistory(h1, detail);
    expect(h2.back).toEqual([home, providers]);
    expect(h2.forward).toEqual([]);
  });

  test("navigating to the same view is a no-op", () => {
    expect(navigateHistory(start, home)).toBe(start);
  });

  test("back and forward walk the stacks", () => {
    const h = navigateHistory(navigateHistory(start, providers), detail);
    const back1 = goBackHistory(h);
    expect(back1.current).toEqual(providers);
    expect(back1.forward).toEqual([detail]);
    const back2 = goBackHistory(back1);
    expect(back2.current).toEqual(home);
    const fwd = goForwardHistory(back2);
    expect(fwd.current).toEqual(providers);
    expect(fwd.forward).toEqual([detail]);
  });

  test("back at the root and forward at the tip are no-ops", () => {
    expect(goBackHistory(start)).toBe(start);
    expect(goForwardHistory(start)).toBe(start);
  });

  test("navigate after back drops the forward branch", () => {
    const h = goBackHistory(navigateHistory(start, providers));
    const h2 = navigateHistory(h, detail);
    expect(h2.forward).toEqual([]);
    expect(h2.current).toEqual(detail);
  });
});
