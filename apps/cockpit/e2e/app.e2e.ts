import { expect, test } from "@playwright/test";
import type { AgentMention, AgentRun, AgentRunRosterInfo, CoreEvent, Message } from "../src/bindings";
import {
  ACCOUNT_CATALOG,
  ACCOUNT_CONNECTIONS,
  DELEGATE_ACTIVE_RUN,
  DELEGATE_DONE_RUN,
  DELEGATION_PARENT_MESSAGE,
  DELETED_OWNER_MESSAGE,
  DELETED_OWNER_SESSION,
  installMockIPC,
  LEGACY_MESSAGE,
  LEGACY_SESSION,
  mockCalls,
  PROVIDER_FAMILY_ROUTE_SELECTIONS,
  REGISTRY_WITHOUT_REVIEWER,
  REVIEWER_CHILD_TRANSCRIPT,
  SESSION,
} from "./mock-ipc";

test.beforeEach(async ({ page }, testInfo) => {
  const dispatchOverrides = testInfo.title.startsWith("agent dispatch:")
    ? {
        list_sessions: [dispatchSession],
        list_messages: [dispatchToolRow()],
        agentRunRoster: roster([
          childRun(testInfo.title.includes("retry") ? { status: "failed", error: "The fixture worker timed out.", finishedAt: 3_000 } : {}),
        ]),
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
        retryChildMessages: testInfo.title.includes("retry") ? { [childRunId]: [retryChildTextRow()] } : {},
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
  const delegationOverrides = testInfo.title.startsWith("delegation:")
    ? {
        agentRunRoster: { rootRunId: null, runs: [DELEGATE_ACTIVE_RUN, DELEGATE_DONE_RUN] },
        childMessages: { [DELEGATE_ACTIVE_RUN.runId]: REVIEWER_CHILD_TRANSCRIPT },
        list_messages: [DELEGATION_PARENT_MESSAGE],
      }
    : {};
  const historyOverrides = testInfo.title.startsWith("history:")
    ? {
        list_agents: REGISTRY_WITHOUT_REVIEWER,
        list_sessions: [LEGACY_SESSION, DELETED_OWNER_SESSION],
        list_messages: [LEGACY_MESSAGE, DELETED_OWNER_MESSAGE],
      }
    : {};
  await installMockIPC(page, {
    ...(testInfo.title === "resolved provider and family changes are durable identity changes"
      ? { route_selections: PROVIDER_FAMILY_ROUTE_SELECTIONS }
      : {}),
    ...accountOverrides,
    ...dispatchOverrides,
    ...delegationOverrides,
    ...historyOverrides,
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
const retryTranscriptText = "The replacement attempt is checking the release checklist from the beginning.";
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

function retryChildTextRow(): Message {
  return {
    ...childTextRow(),
    seq: 1,
    payload: { text: retryTranscriptText },
    createdAt: 2_200,
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

function selectedAgentRunDetail(page: import("@playwright/test").Page) {
  return page.getByRole("button", { name: "Back to Agents" }).locator("xpath=../..");
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
  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "retry_child_run").at(-1)?.args)
    .toMatchObject({
      runId: childRunId,
    });

  await page.reload();
  await page.getByText("Dispatch lifecycle", { exact: true }).click();
  const rehydratedCard = page.getByRole("button", { name: "Open Release Scout agent run" });
  await expect(rehydratedCard).toHaveCount(1);
  await expect(rehydratedCard).toContainText("Queued");
  await expect(rehydratedCard).toContainText("Retry 2");
  await rehydratedCard.click();
  await expect(selectedAgentRunDetail(page).getByText(retryTranscriptText, { exact: true })).toHaveCount(1);
  await expect(selectedAgentRunDetail(page).getByText("The first attempt exceeded its timeout.", { exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "Back to Agents" }).click();
  await expect(page.getByText("Done (1)", { exact: true })).toBeVisible();
  await expect(page.getByText("Active (1)", { exact: true })).toBeVisible();
  const activeRoster = page.getByText("Active (1)", { exact: true }).locator("xpath=..");
  const doneRoster = page.getByText("Done (1)", { exact: true }).locator("xpath=..");
  await expect(activeRoster.getByText("Release Scout", { exact: true })).toHaveCount(1);
  await expect(doneRoster.getByText("Release Scout", { exact: true })).toHaveCount(1);
  await activeRoster.getByText("Release Scout", { exact: true }).click();
  await expect(selectedAgentRunDetail(page).getByText(retryTranscriptText, { exact: true })).toHaveCount(1);
  await expect(selectedAgentRunDetail(page).getByText("The first attempt exceeded its timeout.", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Back to Agents" }).click();
  await doneRoster.getByText("Release Scout", { exact: true }).click();
  await expect(selectedAgentRunDetail(page).getByText("The first attempt exceeded its timeout.", { exact: true })).toHaveCount(1);
  await expect(selectedAgentRunDetail(page).getByText(retryTranscriptText, { exact: true })).toHaveCount(0);
  await expect
    .poll(async () => (await mockCalls(page)).filter((call) => call.cmd === "get_child_transcript").map((call) => call.args?.runId))
    .toEqual(expect.arrayContaining([childRunId, retryRunId]));
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
    await page.getByText("New Task", { exact: true }).first().click();
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

// --- Agentic journeys (Plans 3-5): agent management + start-chat, mention
// delegation + child transcript, legacy/deleted read-only history, and the
// Models route editor's per-target effort contract. Every substitution of a
// stale brief string for the real UI element is called out inline.

test("agents: manage a non-default agent and start a chat session for it", async ({ page }) => {
  await page.goto("/");
  await page.getByText("Agents", { exact: true }).first().click();
  await expect(page.getByText("Main Agent", { exact: true })).toBeVisible();
  await expect(page.getByText("Sub Agent", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Open Reviewer" }).click();
  const tabs = page.getByTestId("agent-detail-tabs");
  for (const label of ["Overview", "Model", "Permissions", "Skills & Tools", "Learning", "Advanced"]) {
    await expect(tabs.getByText(label, { exact: true })).toBeVisible();
  }

  // Only the action menu drives "Start chat" — no separate start button.
  await page.getByRole("button", { name: "Actions for Reviewer" }).click();
  await page.getByTestId("agent-actions-panel").getByRole("button", { name: "Start chat" }).click();
  await expect(page.getByRole("heading", { name: /What should we build/ })).toBeVisible();

  // Substitution: the brief's "New session primary-agent combobox contains
  // Reviewer" doesn't exist — HomeView.tsx has no primary-agent picker at
  // all. openAgentChat records Reviewer via nav.pendingPrimaryAgentId
  // (store-nav.ts), consumed by choosePrimaryAgent; the only observable proof
  // is mentions.ts's matchMentionAgents excluding the CURRENT primary from
  // @-suggestions. With Reviewer primary, "@" now offers Ryuzi, not Reviewer.
  // "@" alone is ambiguous with composer-context.ts's file-reference query
  // (an empty context query is falsy, so its early-return guard doesn't
  // fire, and mentionMenuOpen requires contextQuery === null) — a query
  // character disambiguates, matching how a real user would type it.
  const composer = page.getByPlaceholder("Do anything");
  await composer.fill("@Ry");
  await expect(page.getByRole("menu")).toContainText("Ryuzi");
  await expect(page.getByRole("menu")).not.toContainText("Reviewer");

  // Substitution: "no model/effort/permission/Orchestrate control" — none of
  // these exist anywhere in the Home composer (models are picked per-composer
  // via the Project/Branch pickers only; ProjectSettingsModal.tsx explicitly
  // dropped model/effort/permission fields). Asserted directly for drift.
  await expect(page.getByText("Orchestrate", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: /model/i })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: /effort/i })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: /permission/i })).toHaveCount(0);

  await composer.fill("kick off the review");
  await composer.press("Enter");

  await expect.poll(async () => (await mockCalls(page)).some((c) => c.cmd === "start_chat_session")).toBe(true);
  const start = (await mockCalls(page)).find((c) => c.cmd === "start_chat_session");
  expect(start?.args).toMatchObject({ primaryAgentId: "reviewer", turn: { text: "kick off the review" } });
});

test("delegation: mention-selected child run opens its transcript and returns to the roster", async ({ page }) => {
  await page.goto("/");
  const homeComposer = page.getByPlaceholder("Do anything");
  await homeComposer.fill("investigate the flaky test");
  await homeComposer.press("Enter");
  await expect.poll(async () => (await mockCalls(page)).some((c) => c.cmd === "start_chat_session")).toBe(true);

  // Stop the running turn so the follow-up send goes through continue_session
  // (a running session's Enter/Send instead enqueues via ChatRequestOptions,
  // which has no structured-mentions field — see SessionView.tsx's submit()).
  await page.getByTitle("Stop").click();
  await expect(page.getByTitle("Send")).toBeVisible();

  // A freshly-started session's focusedSession is set directly by store.ts's
  // start/startChat (not via the setFocused action), so hydrateTranscript
  // never fires for it and the seeded parent message wouldn't load. Reload
  // and reselect from the sidebar — same pattern as the existing "route
  // switch notices" test — to force the real hydrate path.
  await page.reload();
  await page.getByText("Untitled session", { exact: true }).click();
  await expect(page.getByText("Kicking off the review delegation.")).toBeVisible();

  const sessionComposer = page.getByPlaceholder("Ask for follow-up changes");
  await sessionComposer.fill("@Rev");
  await expect(page.getByRole("menu")).toContainText("Reviewer");
  await sessionComposer.press("Enter"); // keyboard-selects Reviewer (the only match)
  await expect(sessionComposer).toHaveValue("@Reviewer ");
  await page.getByTitle("Send").click();
  await expect(page.getByTitle("Send")).toBeVisible();

  // Real structured-mention shape confirmed against mentions.ts/bindings.ts's
  // AgentMention — matches the brief's guess exactly, verified rather than
  // assumed.
  const expectedMention = { agentId: "reviewer", labelSnapshot: "Reviewer", startUtf16: 0, endUtf16: 9 } satisfies AgentMention;
  await expect.poll(async () => (await mockCalls(page)).some((c) => c.cmd === "continue_session")).toBe(true);
  const delegateCall = (await mockCalls(page)).find((c) => c.cmd === "continue_session");
  const turn = delegateCall?.args?.turn as { mentions: AgentMention[] } | undefined;
  expect(turn?.mentions).toEqual([expectedMention]);
  expect((await mockCalls(page)).filter((c) => c.cmd === "continue_session")).toHaveLength(1);

  // Substitution: the brief's "Active/Done tabs" are section headings inside
  // AgentRunRoster.tsx, not tabs — "Active (N)" / "Done (N)" `<h3>`s.
  await page.getByTitle("Toggle right panel").click();
  await page.getByTestId("right-panel-header").getByRole("button", { name: "Agents", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Active (1)", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Done (1)", exact: true })).toBeVisible();
  await expect(page.getByText("Main agent", { exact: true })).toBeVisible();
  await expect(page.getByText("Subagent", { exact: true })).toBeVisible();

  await page.getByText("Reviewer", { exact: true }).click();
  await expect(page.getByText("Reviewing the diff for regressions now.")).toBeVisible();

  await page.getByRole("button", { name: "Back to Agents" }).click();
  await expect(page.getByRole("heading", { name: "Active (1)", exact: true })).toBeVisible();
  await expect(page.getByText("Kicking off the review delegation.")).toBeVisible();

  expect((await mockCalls(page)).some((c) => c.cmd === "retry_child_run")).toBe(false);
  expect((await mockCalls(page)).some((c) => c.cmd === "cancel_child_run")).toBe(false);
});

test("history: legacy and deleted-owner sessions stay read-only", async ({ page }) => {
  await page.goto("/");

  await page.getByText("Legacy history", { exact: true }).click();
  // Substitution: "Legacy agent" / "Deleted" labels are derived through the
  // real access logic (lib/session-primary.ts's sessionPrimaryLabel), not
  // fixture-only display fields — a null primaryAgentSnapshot always renders
  // "Legacy agent" regardless of registry contents.
  await expect(page.getByText("Legacy agent", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("This is the preserved legacy transcript.")).toBeVisible();
  await expect(page.getByPlaceholder("Legacy sessions are read-only.")).toBeDisabled();
  await expect(page.getByTitle("Send")).toBeDisabled();
  await expect(page.getByRole("button", { name: "Repair agent" })).toHaveCount(0);
  await expect(page.getByTitle("Stop")).toHaveCount(0);
  // No per-session model/effort/permission control exists in SessionView at
  // all (models are chosen per-composer, not stored per session) — asserted
  // directly rather than skipped.
  await expect(page.getByRole("combobox", { name: /model/i })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: /effort/i })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: /permission/i })).toHaveCount(0);

  await page.getByText("Deleted owner history", { exact: true }).click();
  // sessionPrimaryLabel renders "<name> (Deleted)" as one string when the
  // captured owner is absent from the live registry — REGISTRY_WITHOUT_REVIEWER
  // makes "reviewer" genuinely absent, not a fixture-only deletion flag.
  await expect(page.getByText("Reviewer (Deleted)", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("This is the preserved reviewer transcript.")).toBeVisible();
  await expect(page.getByPlaceholder("The session’s primary agent was deleted, so this session is read-only.")).toBeDisabled();
  await expect(page.getByTitle("Send")).toBeDisabled();
  await expect(page.getByRole("button", { name: "Repair agent" })).toHaveCount(0);
  await expect(page.getByTitle("Stop")).toHaveCount(0);

  const calls = await mockCalls(page);
  for (const forbidden of ["continue_session", "steer_session", "retry_child_run", "cancel_child_run"]) {
    expect(calls.some((c) => c.cmd === forbidden)).toBe(false);
  }
});

test("route effort: preserves an explicit override and clears it for effort-less targets", async ({ page }) => {
  await page.goto("/");
  await page.getByText("Models", { exact: true }).first().click();
  await page.getByRole("button", { name: "Route", exact: true }).click();

  await page.getByRole("button", { name: "New route" }).click();
  await page.getByPlaceholder("smart").fill("smart-route");

  const effortCombobox = page.getByRole("combobox", { name: "Target 1 effort", exact: true });
  await expect(effortCombobox).toBeVisible();
  await effortCombobox.click();
  await page.getByRole("option", { name: "High", exact: true }).click();
  await page.getByRole("button", { name: "Save route" }).click();
  await expect(page.getByText(/High override/)).toBeVisible();

  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "Target 1 effort", exact: true })).toContainText("High");

  const targetPicker = page.getByRole("combobox", { name: "Target 1", exact: true });
  await targetPicker.click();
  await expect(page.getByRole("option", { name: "model-beta", exact: true })).toBeVisible();
  const optionTexts = await page.getByRole("option").allTextContents();
  expect(optionTexts.length).toBeGreaterThan(0);
  for (const text of optionTexts) {
    expect(text).not.toMatch(/-(high|medium|xhigh|low)$/i);
  }
  await page.getByRole("option", { name: "model-beta", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "Target 1 effort", exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "Save route" }).click();
  await expect(page.getByText(/override/)).toHaveCount(0);

  const saves = (await mockCalls(page)).filter((c) => c.cmd === "save_model_route");
  expect(saves).toHaveLength(2);
  const savedRoute = saves[saves.length - 1]?.args?.route as { targets: Array<{ provider: string; model: string; effort: string | null }> };
  expect(savedRoute.targets[0]).toMatchObject({ provider: "fixture", model: "model-beta", effort: null });
});
