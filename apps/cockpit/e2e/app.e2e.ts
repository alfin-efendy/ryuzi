import { expect, test } from "@playwright/test";
import type { AgentRun, AgentRunRosterInfo, CoreEvent, Message } from "../src/bindings";
import {
  ACCOUNT_CATALOG,
  ACCOUNT_CONNECTIONS,
  installMockIPC,
  mockCalls,
  PROVIDER_FAMILY_ROUTE_SELECTIONS,
  SESSION,
} from "./mock-ipc";

test.beforeEach(async ({ page }, testInfo) => {
  const dispatchOverrides = testInfo.title.startsWith("agent dispatch:")
    ? {
        list_sessions: [dispatchSession],
        list_messages: [dispatchToolRow()],
        agentRunRoster: roster([childRun(testInfo.title.includes("retry") ? { status: "failed", error: "The fixture worker timed out.", finishedAt: 3_000 } : {})]),
        childMessages: testInfo.title.includes("retry")
          ? {
              [childRunId]: [
                {
                  ...childTextRow(),
                  payload: { text: "The first attempt exceeded its timeout." },
                },
              ],
            }
          : { [childRunId]: [] },
      }
    : {};
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
    ...dispatchOverrides,
  });
});

async function openProvider(page: import("@playwright/test").Page, name: string) {
  await page.getByText("Models", { exact: true }).first().click();
  await page.getByRole("button", { name: new RegExp(`^${name} \\d+ accounts?`) }).click();
}

async function selectDemoProject(page: import("@playwright/test").Page) {
  await page.getByRole("combobox", { name: "Project" }).click();
  await page.getByRole("option", { name: /demo/ }).click();
}

const dispatchSession = { ...SESSION, sessionPk: "dispatch-session", title: "Dispatch lifecycle", status: "running" as const };
const rootRunId = "primary-dispatch-run";
const dispatchToolCallId = "dispatch-tool-call";
const childRunId = "release-scout-run";
const retryRunId = `${childRunId}-retry`;
const childPreviewText = "Child found the release script.";
const terminalResult = "Release checklist is ready for review.";

function childRun(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runId: childRunId,
    sessionPk: dispatchSession.sessionPk,
    parentRunId: rootRunId,
    retryOf: null,
    sourceToolCallId: dispatchToolCallId,
    dispatchIndex: 0,
    primaryAgentId: "ryuzi",
    executingAgentId: "release-scout",
    executingAgentNameSnapshot: "Release Scout",
    agentKind: "subagent",
    task: "Inspect the release checklist",
    status: "queued",
    startedAt: null,
    finishedAt: null,
    toolCount: 0,
    resolvedModel: "fixture/model-alpha",
    resolvedEffort: "medium",
    result: null,
    error: null,
    ...overrides,
  };
}

function dispatchToolRow(): Message {
  return {
    sessionPk: dispatchSession.sessionPk,
    seq: 1,
    role: "assistant",
    blockType: "tool_call",
    payload: { name: "task", input: { prompt: "Inspect the release checklist" } },
    toolCallId: dispatchToolCallId,
    status: "in_progress",
    toolKind: "task",
    createdAt: 1_000,
    speaker: null,
  };
}

function roster(runs: AgentRun[]): AgentRunRosterInfo {
  return { rootRunId, runs };
}

function childToolRow(): Message {
  return {
    sessionPk: dispatchSession.sessionPk,
    seq: 1,
    role: "assistant",
    blockType: "tool_call",
    payload: { name: "read", input: { path: "package.json" } },
    toolCallId: "release-scout-read-package",
    status: "completed",
    toolKind: "read",
    createdAt: 2_000,
    speaker: null,
  };
}

function childTextRow(): Message {
  return {
    sessionPk: dispatchSession.sessionPk,
    seq: 2,
    role: "assistant",
    blockType: "text",
    payload: { text: childPreviewText },
    toolCallId: null,
    status: null,
    toolKind: null,
    createdAt: 2_100,
    speaker: null,
  };
}

type MockCoreEventInput = {
  event: CoreEvent;
  roster?: AgentRunRosterInfo;
  childMessage?: { runId: string; message: Message };
};

async function emitMockCoreEvent(page: import("@playwright/test").Page, input: MockCoreEventInput): Promise<void> {
  await page.evaluate((payload) => {
    (window as unknown as { __emitMockCoreEvent: (input: MockCoreEventInput) => void }).__emitMockCoreEvent(payload);
  }, input);
}

test("agent dispatch: lifecycle cards hydrate, stream, complete, and reload", async ({ page }) => {
  await page.goto("/");
  await page.getByText("Dispatch lifecycle", { exact: true }).click();

  const card = page.getByRole("button", { name: "Open Release Scout agent run" });
  await expect(card).toHaveCount(1);
  await expect(card).toContainText("Queued");
  await expect(page.getByText("task", { exact: true })).toHaveCount(0);

  const runningRun = childRun({ status: "running", startedAt: 2_000 });
  await emitMockCoreEvent(page, {
    roster: roster([runningRun]),
    event: {
      kind: "agentRunChanged",
      session_pk: dispatchSession.sessionPk,
      run_id: childRunId,
      parent_run_id: rootRunId,
      status: "running",
    },
  });
  await expect(card).toContainText("Running");

  await emitMockCoreEvent(page, {
    childMessage: { runId: childRunId, message: childToolRow() },
    event: {
      kind: "agentRunMessage",
      session_pk: dispatchSession.sessionPk,
      run_id: childRunId,
      seq: 1,
      role: "assistant",
      block_type: "tool_call",
      payload: childToolRow().payload,
      tool_call_id: childToolRow().toolCallId,
      status: childToolRow().status,
      tool_kind: childToolRow().toolKind,
      speaker: null,
    },
  });
  await emitMockCoreEvent(page, {
    childMessage: { runId: childRunId, message: childTextRow() },
    event: {
      kind: "agentRunMessage",
      session_pk: dispatchSession.sessionPk,
      run_id: childRunId,
      seq: 2,
      role: "assistant",
      block_type: "text",
      payload: childTextRow().payload,
      tool_call_id: null,
      status: null,
      tool_kind: null,
      speaker: null,
    },
  });

  await expect(card).toContainText("read · package.json");
  await expect(card).toContainText(childPreviewText);
  await expect(page.getByText("read", { exact: true })).toHaveCount(0);

  await card.click();
  await expect(page.getByTestId("right-panel-header").getByRole("button", { name: "Agents" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Back to Agents" })).toBeVisible();
  await page.getByRole("button", { name: "See 1 step" }).click();
  await expect(page.getByText("read", { exact: true })).toHaveCount(1);
  await expect(page.getByText("package.json", { exact: true })).toHaveCount(1);
  await expect(page.getByText(childPreviewText, { exact: true })).toHaveCount(2);

  const completedRun = childRun({ status: "completed", startedAt: 2_000, finishedAt: 4_000, result: terminalResult, toolCount: 1 });
  await emitMockCoreEvent(page, {
    roster: roster([completedRun]),
    event: {
      kind: "agentRunChanged",
      session_pk: dispatchSession.sessionPk,
      run_id: childRunId,
      parent_run_id: rootRunId,
      status: "completed",
    },
  });
  await expect(card).toContainText(terminalResult);

  await page.reload();
  await page.getByText("Dispatch lifecycle", { exact: true }).click();
  const rehydratedCard = page.getByRole("button", { name: "Open Release Scout agent run" });
  await expect(rehydratedCard).toHaveCount(1);
  await expect(rehydratedCard).toContainText("Completed");
  await expect(rehydratedCard).toContainText(terminalResult);
  await rehydratedCard.click();
  await expect(page.getByText(terminalResult, { exact: true }).last()).toBeVisible();
  await expect(page.getByText(childPreviewText, { exact: true }).last()).toBeVisible();
});

test("agent dispatch: retry keeps one card slot and both durable attempts", async ({ page }) => {
  await page.goto("/");
  await page.getByText("Dispatch lifecycle", { exact: true }).click();

  const initialCard = page.getByRole("button", { name: "Open Release Scout agent run" });
  await expect(initialCard).toHaveCount(1);
  await initialCard.click();
  await expect(page.getByText("The first attempt exceeded its timeout.", { exact: true })).toHaveCount(1);
  await page.getByRole("button", { name: "Retry" }).click();

  const retryCard = page.getByRole("button", { name: "Open Release Scout agent run" });
  await expect(retryCard).toHaveCount(1);
  await expect(retryCard).toContainText("Queued");
  await expect(retryCard).toContainText("Retry 2");
  await expect.poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "retry_child_run").at(-1)?.args).toMatchObject({
    runId: childRunId,
  });

  await expect(page.getByRole("button", { name: "Back to Agents" })).toBeVisible();
  await page.getByRole("button", { name: "Back to Agents" }).click();
  await expect(page.getByText("Done (1)", { exact: true })).toBeVisible();
  await expect(page.getByText("Active (1)", { exact: true })).toBeVisible();
  await page.getByText("Release Scout", { exact: true }).last().click();
  await expect(page.getByText("The first attempt exceeded its timeout.", { exact: true })).toHaveCount(1);
  await page.getByRole("button", { name: "Back to Agents" }).click();
  await page.getByText("Release Scout", { exact: true }).first().click();
  await expect(page.getByRole("button", { name: "Back to Agents" })).toBeVisible();
  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "get_child_transcript").map((call) => call.args?.runId))
    .toEqual(expect.arrayContaining([childRunId, retryRunId]));
  expect((await mockCalls(page)).filter((call) => call.cmd === "get_child_transcript").at(-1)?.args).toMatchObject({ runId: retryRunId });
});

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
  for (const label of ["Models", "Automations", "Plugins", "Settings"]) {
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
  expect(start?.args).toMatchObject({ turn: { text: "build me a test" } });
});

test("composer Enter with a project selected starts a project session", async ({ page }) => {
  await page.goto("/");
  // Home defaults to no project, so select the demo project first; then Enter
  // starts a project session bound to it.
  await selectDemoProject(page);
  const composer = page.getByPlaceholder("Do anything");
  await composer.fill("build me a test");
  await composer.press("Enter");
  await expect.poll(async () => (await mockCalls(page)).some((c) => c.cmd === "start_session")).toBe(true);
  await expect(page.getByRole("heading", { name: /What should we build/ })).toHaveCount(0);
  const start = (await mockCalls(page)).find((c) => c.cmd === "start_session");
  expect(start?.args).toMatchObject({ projectId: "p-demo", turn: { text: "build me a test" } });
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

test("route switch notices render live once and survive reload", async ({ page }) => {
  await page.goto("/");
  await page.getByPlaceholder("Do anything").fill("establish route baseline");
  await page.getByTitle("Start session").click();

  const switchNotices = page.getByText(/^(Switched to|Account switched to)/);
  await expect(switchNotices).toHaveCount(0);
  await page.getByTitle("Stop").click();
  await expect(page.getByTitle("Send")).toBeVisible();

  const sendTurn = async (text: string) => {
    await page.getByPlaceholder("Ask for follow-up changes").fill(text);
    await page.getByTitle("Send").click();
    await expect(page.getByTitle("Send")).toBeVisible();
  };

  await sendTurn("use the same route");
  await expect(switchNotices).toHaveCount(0);

  await sendTurn("rotate the account");
  await expect(page.getByText("Account switched to Backup account · round robin", { exact: true })).toHaveCount(1);

  await sendTurn("keep this route");
  await expect(switchNotices).toHaveCount(1);

  await page.reload();
  await page.getByText("Untitled session", { exact: true }).click();
  await expect(page.getByText("Account switched to Backup account · round robin", { exact: true })).toHaveCount(1);
  await expect(switchNotices).toHaveCount(1);
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
