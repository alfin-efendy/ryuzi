import { expect, test } from "@playwright/test";
import { installMockIPC, mockCalls } from "./mock-ipc";

test.beforeEach(async ({ page }) => {
  await installMockIPC(page);
});

test("boots to Home with the project loaded", async ({ page }) => {
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: /What should we build/ }),
  ).toBeVisible();
  const calls = await mockCalls(page);
  expect(calls.some((c) => c.cmd === "list_projects")).toBe(true);
  expect(calls.some((c) => c.cmd === "list_sessions")).toBe(true);
});
