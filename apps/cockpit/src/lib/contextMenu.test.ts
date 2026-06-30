import { test, expect } from "bun:test";
import { shouldSuppressContextMenu } from "./contextMenu";

test("suppresses in production, allows in dev", () => {
  expect(shouldSuppressContextMenu(false)).toBe(true);
  expect(shouldSuppressContextMenu(true)).toBe(false);
});
