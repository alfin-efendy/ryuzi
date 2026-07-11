import { expect, test } from "@playwright/test";
import { ACCOUNT_CATALOG, ACCOUNT_CONNECTIONS, installMockIPC, mockCalls, PROVIDER_FAMILY_ROUTE_SELECTIONS } from "./mock-ipc";

test.beforeEach(async ({ page }, testInfo) => {
  const accountOverrides = testInfo.title.startsWith("accounts:")
    ? {
        list_provider_catalog: ACCOUNT_CATALOG,
        list_connections: ACCOUNT_CONNECTIONS,
        ...(testInfo.title.includes("quota isolation") ? { quota_failure_once: "claude-personal" } : {}),
        ...(testInfo.title.includes("late quota") ? { delayed_quota: "codex-primary" } : {}),
      }
    : {};
  await installMockIPC(page, {
    ...(testInfo.title === "resolved provider and family changes are durable identity changes"
      ? { route_selections: PROVIDER_FAMILY_ROUTE_SELECTIONS }
      : {}),
    ...accountOverrides,
  });
});

async function openProvider(page: import("@playwright/test").Page, name: string) {
  await page.getByText("Models", { exact: true }).first().click();
  await page.getByRole("button", { name: new RegExp(`^${name} \\d+ accounts?`) }).click();
}

test("boots to Home with the project loaded", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /What should we build/ })).toBeVisible();
  const calls = await mockCalls(page);
  expect(calls.some((c) => c.cmd === "list_projects")).toBe(true);
  expect(calls.some((c) => c.cmd === "list_sessions")).toBe(true);
});

test("connection fixtures match the public account contract", async ({ page }) => {
  await page.goto("/");
  const connections = await page.evaluate(async () => {
    const tauri = (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args: unknown) => Promise<unknown> } })
      .__TAURI_INTERNALS__;
    return (await tauri.invoke("list_connections", {})) as Array<Record<string, unknown>>;
  });

  expect(connections).toHaveLength(1);
  expect(connections[0]).toMatchObject({ quotaCapability: null, models: ["model-alpha", "model-beta"], needsRelogin: false });
  expect(connections[0]).not.toHaveProperty("baseUrl");
  expect(connections[0]).not.toHaveProperty("keyMasked");
  expect(connections[0]).not.toHaveProperty("claudeCloaking");
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

test("accounts: quota isolation, retry, and capability-driven rendering", async ({ page }) => {
  await page.goto("/");
  await openProvider(page, "Claude Code");

  await expect(page.getByRole("button", { name: "Retry quota for Claude Personal" })).toBeVisible();
  await page.getByRole("button", { name: "Retry quota for Claude Personal" }).click();
  await expect(page.getByRole("progressbar", { name: "Claude Personal 5 hour quota" })).toHaveAttribute("aria-valuenow", "20");

  await page.getByRole("button", { name: "Models" }).first().click();
  await page.getByRole("button", { name: /^Codex 2 accounts/ }).click();
  await expect(page.getByRole("progressbar", { name: "Codex Primary Codex primary quota" })).toHaveAttribute("aria-valuenow", "20");
  await expect(page.getByRole("progressbar", { name: "Codex Backup Codex primary quota" })).toHaveAttribute("aria-valuenow", "35");
  await expect(page.getByText("2 reset credits available")).toHaveCount(2);

  await page.getByRole("button", { name: "Models" }).first().click();
  await page.getByRole("button", { name: /^Kiro 1 account/ }).click();
  await expect(page.getByText("Quota", { exact: true })).toHaveCount(0);
  const calls = await mockCalls(page);
  expect(calls.filter((call) => call.cmd === "connection_provider_quota").map((call) => call.args?.id)).toEqual([
    "claude-personal",
    "claude-personal",
    "codex-primary",
    "codex-backup",
  ]);
});

test("accounts: inline rename, switch, reorder, test, reset, and delete remain on provider list", async ({ page }) => {
  await page.goto("/");
  await openProvider(page, "Codex");
  await expect(page.getByText("Codex Primary", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Rename Codex Primary" }).click();
  const rename = page.getByRole("dialog", { name: "Rename account" });
  await expect(rename.getByRole("textbox")).toHaveCount(1);
  await expect(rename.getByRole("textbox", { name: "Account name" })).toBeVisible();
  await expect(rename.getByRole("textbox", { name: /API key/i })).toHaveCount(0);
  await expect(rename.getByRole("textbox", { name: /Base URL/i })).toHaveCount(0);
  await expect(rename.getByText(/credential|secret|cloaking/i)).toHaveCount(0);
  await rename.getByRole("textbox", { name: "Account name" }).fill("  Main Codex  ");
  await rename.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Main Codex", { exact: true })).toBeVisible();

  await page.getByRole("switch", { name: "Enabled Main Codex" }).click();
  await expect(page.getByRole("switch", { name: "Enabled Main Codex" })).toHaveAttribute("aria-checked", "false");
  await page.getByRole("button", { name: "Move Codex Backup up" }).click();
  await expect
    .poll(async () =>
      page.getByRole("button", { name: /^Rename / }).evaluateAll((buttons) => buttons.map((button) => button.getAttribute("aria-label"))),
    )
    .toEqual(["Rename Codex Backup", "Rename Main Codex"]);
  await page.getByRole("button", { name: "Test Codex Backup" }).click();
  await expect(page.getByText("Connection works", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Reset credit for Codex Backup" }).click();
  const reset = page.getByRole("dialog", { name: "Reset credit?" });
  await expect(reset).toContainText("Codex Backup");
  await reset.getByRole("button", { name: "Reset credit" }).click();
  await expect(reset).toHaveCount(0);

  await page.getByRole("button", { name: "Delete Codex Backup" }).click();
  const remove = page.getByRole("dialog", { name: "Delete account?" });
  await expect(remove).toContainText("Codex Backup");
  await remove.getByRole("button", { name: "Delete account" }).click();
  await expect(page.getByText("Codex Backup", { exact: true })).toHaveCount(0);

  await expect(page.getByTitle("Details")).toHaveCount(0);
  await expect(page.getByText(/^(API key|OAuth|\d+ ACTIVE|Cloaking)$/i)).toHaveCount(0);
  const calls = await mockCalls(page);
  expect(calls.some((call) => call.cmd === "rename_connection" && call.args?.label === "Main Codex")).toBe(true);
  expect(calls.some((call) => call.cmd === "set_connection_enabled" && call.args?.enabled === false)).toBe(true);
  expect(calls.some((call) => call.cmd === "move_connection" && call.args?.id === "codex-backup")).toBe(true);
  expect(calls.some((call) => call.cmd === "test_connection" && call.args?.id === "codex-backup")).toBe(true);
  expect(calls.some((call) => call.cmd === "reset_codex_credit" && call.args?.id === "codex-backup")).toBe(true);
  expect(calls.some((call) => call.cmd === "remove_connection" && call.args?.id === "codex-backup")).toBe(true);
});

test("accounts: redirect reconnect stays in place and device reconnect opens Add account", async ({ page }) => {
  await page.goto("/");
  await openProvider(page, "Claude Code");
  await page.getByRole("button", { name: "Reconnect Claude Personal" }).click();
  await expect
    .poll(async () =>
      (await mockCalls(page)).some((call) => call.cmd === "reconnect_oauth" && call.args?.connectionId === "claude-personal"),
    )
    .toBe(true);
  await expect(page.getByRole("heading", { name: "Claude Code" })).toBeVisible();

  await page.getByRole("button", { name: "Models" }).first().click();
  await page.getByRole("button", { name: /^Kiro 1 account/ }).click();
  await page.getByRole("button", { name: "Reconnect Kiro Device" }).click();
  await expect(page.getByRole("dialog", { name: "Add account" })).toBeVisible();
  expect((await mockCalls(page)).some((call) => call.cmd === "reconnect_oauth" && call.args?.connectionId === "kiro-device")).toBe(false);
});

test("accounts: late quota cannot repopulate an unmounted provider and reload starts clean", async ({ page }) => {
  await page.goto("/");
  await openProvider(page, "Codex");
  await expect(page.getByText("Loading quota…").first()).toBeVisible();

  await page.getByRole("button", { name: "Models" }).first().click();
  await page.evaluate(() => {
    (window as unknown as { __resolveMockQuota: (id: string) => void }).__resolveMockQuota("codex-primary");
  });
  await expect(page.getByRole("progressbar", { name: "Codex Primary Codex primary quota" })).toHaveCount(0);

  await page.getByRole("button", { name: /^Codex 2 accounts/ }).click();
  await expect(page.getByRole("progressbar", { name: "Codex Primary Codex primary quota" })).toHaveAttribute("aria-valuenow", "20");
  await expect(page.getByRole("progressbar", { name: "Codex Primary Codex primary quota" })).not.toHaveAttribute("aria-valuenow", "99");
  await expect(page.getByRole("button", { name: "Retry quota for Codex Primary" })).toHaveCount(0);
  await page.reload();
  await openProvider(page, "Codex");
  await expect(page.getByRole("progressbar", { name: "Codex Primary Codex primary quota" })).toHaveAttribute("aria-valuenow", "20");
});
