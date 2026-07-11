import { expect, test } from "@playwright/test";
import { installMockIPC, mockCalls, PROVIDER_FAMILY_ROUTE_SELECTIONS } from "./mock-ipc";

test.beforeEach(async ({ page }, testInfo) => {
  await installMockIPC(
    page,
    testInfo.title === "resolved provider and family changes are durable identity changes"
      ? { route_selections: PROVIDER_FAMILY_ROUTE_SELECTIONS }
      : {},
  );
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

test("composer Enter starts a session and navigates to it", async ({ page }) => {
  await page.goto("/");
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

test("structured model effort choices follow the selected execution surface", async ({ page }) => {
  await page.goto("/");

  const trigger = page.getByRole("button", { name: "Model and effort" });
  await trigger.click();
  await page.getByText("Model Alpha", { exact: true }).click();

  await trigger.click();
  await expect(page.getByText("Light", { exact: true })).toBeVisible();
  await expect(page.getByText("Medium", { exact: true })).toBeVisible();
  await expect(page.getByText("High", { exact: true })).toBeVisible();
  await expect(page.getByText("Extra high", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Ultra", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Advanced", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Speed", { exact: true })).toHaveCount(0);
  await page.getByText("Medium", { exact: true }).click();

  await trigger.click();
  await page.getByText("Model Beta", { exact: true }).click();
  await trigger.click();
  await expect(page.getByText("Light", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Medium", { exact: true })).toHaveCount(0);
  await expect(page.getByText("High", { exact: true })).toBeVisible();
  await expect(page.getByText("Extra high", { exact: true })).toBeVisible();
  await expect(page.getByText("Ultra", { exact: true })).toBeVisible();

  await page.getByText("Named safe route", { exact: true }).click();
  await trigger.click();
  await expect(page.getByText("High", { exact: true })).toBeVisible();
  await expect(page.getByText("Read-only effort", { exact: true })).toBeVisible();
  await expect(page.getByText("Light", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Medium", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Extra high", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Ultra", { exact: true })).toHaveCount(0);
});

test("project effort override can return to the model default", async ({ page }) => {
  await page.goto("/");
  const trigger = page.getByRole("button", { name: "Model and effort" });
  await trigger.click();
  await page.getByText("Model Alpha", { exact: true }).click();
  await trigger.click();
  await page.getByText("High", { exact: true }).click();
  await trigger.click();
  await page.getByText(/Model default/).click();

  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "update_project_runtime").at(-1)?.args)
    .toMatchObject({ projectId: "p-demo", model: "fixture/model-alpha", effort: null });

  const readsBeforeRemount = (await mockCalls(page)).filter((call) => call.cmd === "project_runtime_info").length;
  await page.getByText("Models", { exact: true }).first().click();
  await page.getByText("New session", { exact: true }).first().click();
  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "project_runtime_info").length)
    .toBeGreaterThan(readsBeforeRemount);
  await expect(trigger).toContainText("Model Alpha");
  await expect(trigger).toContainText("Model default");
});

test("provider screen writes the global default for a concrete model", async ({ page }) => {
  await page.goto("/");
  await page.getByText("Models", { exact: true }).first().click();
  await page.getByRole("button", { name: /Fixture Provider/ }).click();
  await page.getByRole("combobox", { name: "Default effort for Model Alpha" }).click();
  await page.getByRole("option", { name: "High" }).click();

  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "set_model_effort_preference").at(-1)?.args)
    .toMatchObject({ key: { family: "fixture", model: "model-alpha" }, effort: "high" });
});

test("running session effort changes are marked for the next turn", async ({ page }) => {
  await page.goto("/");
  const homeTrigger = page.getByRole("button", { name: "Model and effort" });
  await homeTrigger.click();
  await page.getByText("Model Beta", { exact: true }).click();
  await page.getByPlaceholder("Do anything").fill("start fixture session");
  await page.getByTitle("Start session").click();

  const sessionTrigger = page.getByRole("button", { name: "Model and effort" });
  await sessionTrigger.click();
  await expect(page.getByText("Changes apply to this project’s next turns.", { exact: true })).toBeVisible();
  await page.getByText("Ultra", { exact: true }).click();
  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "update_project_runtime").at(-1)?.args)
    .toMatchObject({ projectId: "p-demo", model: "fixture/model-beta", effort: "ultra" });
});

test("route switch notices render live once and survive reload", async ({ page }) => {
  await page.goto("/");

  const homeTrigger = page.getByRole("button", { name: "Model and effort" });
  await homeTrigger.click();
  await page.getByText("Model Alpha", { exact: true }).click();
  await homeTrigger.click();
  await page.getByText("High", { exact: true }).click();
  await page.getByPlaceholder("Do anything").fill("establish route baseline");
  await page.getByTitle("Start session").click();

  const switchNotices = page.getByText(/^(Switched to|Account switched to)/);
  await expect(switchNotices).toHaveCount(0);
  await page.getByTitle("Stop").click();
  await expect(page.getByTitle("Send")).toBeVisible();

  const sessionTrigger = page.getByRole("button", { name: "Model and effort" });
  await sessionTrigger.click();
  await page.getByText("Model Beta", { exact: true }).click();
  await sessionTrigger.click();
  await page.getByText("Ultra", { exact: true }).click();

  const sendTurn = async (text: string) => {
    await page.getByPlaceholder("Ask for follow-up changes").fill(text);
    await page.getByTitle("Send").click();
    await expect(page.getByTitle("Send")).toBeVisible();
  };

  await sendTurn("use the new model");
  await expect(page.getByText("Switched to Model Beta · Ultra", { exact: true })).toHaveCount(1);

  await sendTurn("rotate the account");
  await expect(page.getByText("Account switched to Backup account · round robin", { exact: true })).toHaveCount(1);

  await sendTurn("keep this route");
  await expect(switchNotices).toHaveCount(2);

  await page.reload();
  await page.getByText("Untitled session", { exact: true }).click();
  await expect(page.getByText("Switched to Model Beta · Ultra", { exact: true })).toHaveCount(1);
  await expect(page.getByText("Account switched to Backup account · round robin", { exact: true })).toHaveCount(1);
  await expect(switchNotices).toHaveCount(2);
});

test("resolved provider and family changes are durable identity changes", async ({ page }) => {
  await page.goto("/");
  await page.getByPlaceholder("Do anything").fill("establish resolved route baseline");
  await page.getByTitle("Start session").click();
  await page.getByTitle("Stop").click();

  const sendTurn = async (text: string) => {
    await page.getByPlaceholder("Ask for follow-up changes").fill(text);
    await page.getByTitle("Send").click();
    await expect(page.getByTitle("Send")).toBeVisible();
  };

  const combined = page.getByText("Switched to Shared Model · High via Fixture account · round robin", { exact: true });
  await sendTurn("change only the resolved provider and family");
  await expect(combined).toHaveCount(1);

  await sendTurn("change only mutable aliases and labels");
  await expect(page.getByText(/^(Switched to|Account switched to)/)).toHaveCount(1);

  await page.reload();
  await page.getByText("Untitled session", { exact: true }).click();
  await expect(combined).toHaveCount(1);
  await expect(page.getByText(/^(Switched to|Account switched to)/)).toHaveCount(1);
});
