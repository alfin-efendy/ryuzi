import { expect, test } from "@playwright/test";
import { installMockIPC, mockCalls } from "./mock-ipc";

test.beforeEach(async ({ page }) => {
  await installMockIPC(page);
});

test("boots to Home with the project loaded", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /What should we build/ })).toBeVisible();
  const calls = await mockCalls(page);
  expect(calls.some((c) => c.cmd === "list_projects")).toBe(true);
  expect(calls.some((c) => c.cmd === "list_sessions")).toBe(true);
});

test("sidebar navigation leaves the Home view", async ({ page }) => {
  await page.goto("/");
  const homeHeading = page.getByRole("heading", { name: /What should we build/ });
  await expect(homeHeading).toBeVisible();
  for (const label of ["Models", "Runtime", "Scheduler", "Plugins", "Settings"]) {
    await page.getByText(label, { exact: true }).first().click();
    await expect(homeHeading).toHaveCount(0);
    // back to Home for the next iteration
    await page.getByText("New session", { exact: true }).first().click();
    await expect(homeHeading).toBeVisible();
  }
});

test("composer Enter with no project starts a chat session and navigates to it", async ({ page }) => {
  await page.goto("/");
  // Home is chat-first: with no project selected (the default), Enter starts a
  // project-less chat session.
  const composer = page.getByPlaceholder("Do anything");
  await composer.fill("build me a test");
  await composer.press("Enter");
  await expect.poll(async () => (await mockCalls(page)).some((c) => c.cmd === "start_chat_session")).toBe(true);
  await expect(page.getByRole("heading", { name: /What should we build/ })).toHaveCount(0);
  const start = (await mockCalls(page)).find((c) => c.cmd === "start_chat_session");
  expect(start?.args).toMatchObject({ prompt: "build me a test" });
});

test("composer Enter with a project selected starts a project session", async ({ page }) => {
  await page.goto("/");
  // Home defaults to no project, so select the demo project first; then Enter
  // starts a project session bound to it.
  await page.getByRole("combobox", { name: "Project" }).click();
  await page.getByRole("option", { name: /demo/ }).click();
  const composer = page.getByPlaceholder("Do anything");
  await composer.fill("build me a test");
  await composer.press("Enter");
  await expect.poll(async () => (await mockCalls(page)).some((c) => c.cmd === "start_session")).toBe(true);
  await expect(page.getByRole("heading", { name: /What should we build/ })).toHaveCount(0);
  const start = (await mockCalls(page)).find((c) => c.cmd === "start_session");
  expect(start?.args).toMatchObject({
    projectId: "p-demo",
    prompt: "build me a test",
  });
});
