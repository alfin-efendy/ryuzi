import { describe, expect, test } from "bun:test";
import { distanceFromBottom, isStuck, pinningInterrupted, showScrollFab } from "./scroll";

describe("scroll stickiness", () => {
  test("distance math", () => {
    expect(distanceFromBottom(1000, 600, 400)).toBe(0);
    expect(distanceFromBottom(1000, 500, 400)).toBe(100);
  });
  test("distance math clamps negative overshoot to zero", () => {
    expect(distanceFromBottom(1000, 700, 400)).toBe(0);
  });
  test("stuck within 40px of the bottom", () => {
    expect(isStuck(0)).toBe(true);
    expect(isStuck(39)).toBe(true);
    expect(isStuck(40)).toBe(false);
  });
  test("FAB appears past 160px", () => {
    expect(showScrollFab(160)).toBe(false);
    expect(showScrollFab(161)).toBe(true);
    expect(showScrollFab(0)).toBe(false);
  });
  test("pinning interrupted when distance grows beyond rounding tolerance", () => {
    expect(pinningInterrupted(100, 102)).toBe(true);
  });
  test("pinning not interrupted when distance shrinks", () => {
    expect(pinningInterrupted(100, 50)).toBe(false);
  });
  test("pinning not interrupted when distance is equal or within 1px tolerance", () => {
    expect(pinningInterrupted(100, 100)).toBe(false);
    expect(pinningInterrupted(100, 101)).toBe(false);
  });
});
