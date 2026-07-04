import { describe, expect, test } from "bun:test";
import { goBackHistory, goForwardHistory, navigateHistory, type NavHistory, type View } from "./store-nav";

const home: View = { kind: "home" };
const models: View = { kind: "models" };
const detail: View = { kind: "connectionDetail", id: "c1" };

const start: NavHistory = { back: [], current: home, forward: [] };

describe("nav history", () => {
  test("navigate pushes current onto back and clears forward", () => {
    const h1 = navigateHistory(start, models);
    expect(h1.current).toEqual(models);
    expect(h1.back).toEqual([home]);
    const h2 = navigateHistory(h1, detail);
    expect(h2.back).toEqual([home, models]);
    expect(h2.forward).toEqual([]);
  });

  test("navigating to the same view is a no-op", () => {
    expect(navigateHistory(start, home)).toBe(start);
  });

  test("back and forward walk the stacks", () => {
    const h = navigateHistory(navigateHistory(start, models), detail);
    const back1 = goBackHistory(h);
    expect(back1.current).toEqual(models);
    expect(back1.forward).toEqual([detail]);
    const back2 = goBackHistory(back1);
    expect(back2.current).toEqual(home);
    const fwd = goForwardHistory(back2);
    expect(fwd.current).toEqual(models);
    expect(fwd.forward).toEqual([detail]);
  });

  test("back at the root and forward at the tip are no-ops", () => {
    expect(goBackHistory(start)).toBe(start);
    expect(goForwardHistory(start)).toBe(start);
  });

  test("navigate after back drops the forward branch", () => {
    const h = goBackHistory(navigateHistory(start, models));
    const h2 = navigateHistory(h, detail);
    expect(h2.forward).toEqual([]);
    expect(h2.current).toEqual(detail);
  });
});

import { sanitizeRightTab, clampPanelSize, RIGHT_WIDTH, BOTTOM_HEIGHT } from "./store-nav";

test("sanitizeRightTab keeps valid tabs and maps legacy/unknown to review", () => {
  expect(sanitizeRightTab("file")).toBe("file");
  expect(sanitizeRightTab("review")).toBe("review");
  expect(sanitizeRightTab("term")).toBe("review"); // legacy persisted value
  expect(sanitizeRightTab(null)).toBe("review");
});

test("clampPanelSize clamps to min and viewport fraction", () => {
  expect(clampPanelSize(100, 1600, RIGHT_WIDTH)).toBe(320);
  expect(clampPanelSize(560, 1600, RIGHT_WIDTH)).toBe(560);
  expect(clampPanelSize(5000, 1600, RIGHT_WIDTH)).toBe(1280); // 80% of 1600
  expect(clampPanelSize(50, 900, BOTTOM_HEIGHT)).toBe(120);
  expect(clampPanelSize(1000, 900, BOTTOM_HEIGHT)).toBe(540); // 60% of 900
});
