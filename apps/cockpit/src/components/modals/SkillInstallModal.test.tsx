import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, InstalledSkillPack, Result, SkillInstallBegin, TrustPromptDto } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

// The modal talks only to the Tauri IPC boundary (`@/bindings`) and the real
// `usePlugins` zustand store (for `load()` after a successful install) —
// mock the boundary, reset the store around each test.

const trustPrompt: TrustPromptDto = {
  token: "t",
  sourceSpec: "acme/p",
  ownerRepo: "acme/p",
  resolvedCommit: "c1",
  skills: ["S"],
  hookScripts: ["tool.before/g.sh"],
  totalBytes: 12,
  runsCode: false,
  curated: false,
};

const installedPack: InstalledSkillPack = {
  id: "acme-p",
  name: "Acme Pack",
  source: "acme/p",
  pluginId: null,
  installedAt: "2026-07-11T00:00:00Z",
  skills: [{ id: "acme-p:s", name: "S" }],
};

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });
const err = (message: string) => Promise.resolve({ status: "error" as const, error: { message } });

const beginSkillInstall = mock(
  (_source: string): Promise<Result<SkillInstallBegin, CmdError>> => ok({ completed: false, trust: trustPrompt, plugin: null }),
);
const confirmSkillInstall = mock((_token: string): Promise<Result<InstalledSkillPack, CmdError>> => ok(installedPack));
const listPlugins = mock(() => ok([]));
const pluginsRestartRequired = mock(() => ok(false));
const catalogStatus = mock(() => ok({ sequence: 0, lastFetchAt: null, outcome: null, entries: 0, blocked: 0 }));
const toastError = mock((_message: string) => {});
const toastSuccess = mock((_message: string) => {});

mock.module("@/bindings", () => ({
  commands: {
    beginSkillInstall,
    confirmSkillInstall,
    listPlugins,
    pluginsRestartRequired,
    catalogStatus,
  },
}));
mock.module("sonner", () => ({
  toast: { error: toastError, success: toastSuccess, info: mock(() => {}), warning: mock(() => {}) },
  Toaster: () => null,
}));

const { SkillInstallModal } = await import("./SkillInstallModal");
const { usePlugins } = await import("@/store-plugins");

const onClose = mock(() => {});

async function renderSkillWizard(initialSource?: string) {
  const result = render(<SkillInstallModal initialSource={initialSource} onClose={onClose} />);
  await act(async () => {});
  return result;
}

beforeEach(() => {
  beginSkillInstall.mockClear();
  beginSkillInstall.mockImplementation((_source: string) => ok({ completed: false, trust: trustPrompt, plugin: null }));
  confirmSkillInstall.mockClear();
  confirmSkillInstall.mockImplementation((_token: string) => ok(installedPack));
  listPlugins.mockClear();
  pluginsRestartRequired.mockClear();
  catalogStatus.mockClear();
  toastError.mockClear();
  toastSuccess.mockClear();
  onClose.mockClear();
  usePlugins.setState({ plugins: [], loaded: false });
});

afterEach(() => {
  cleanup();
  usePlugins.setState({ plugins: [], loaded: false });
});

test("arbitrary source shows the trust-ack step before installing", async () => {
  beginSkillInstall.mockResolvedValueOnce({
    status: "ok",
    data: {
      completed: false,
      trust: {
        token: "t",
        sourceSpec: "acme/p",
        ownerRepo: "acme/p",
        resolvedCommit: "c1",
        skills: ["S"],
        hookScripts: ["tool.before/g.sh"],
        totalBytes: 12,
        runsCode: false,
        curated: false,
      },
      plugin: null,
    },
  });
  await renderSkillWizard("acme/p");
  expect(await screen.findByText(/acme\/p/)).toBeTruthy();
  expect(screen.getByText(/tool\.before\/g\.sh/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Install|Trust/ }));
  await waitFor(() => expect(confirmSkillInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "t"));
});

test("shows the manual source-entry step when no initialSource is given", async () => {
  await renderSkillWizard();

  expect(beginSkillInstall).not.toHaveBeenCalled();
  const input = screen.getByLabelText("Skill source") as HTMLInputElement;
  expect((screen.getByRole("button", { name: "Install" }) as HTMLButtonElement).disabled).toBe(true);

  fireEvent.change(input, { target: { value: "obra/superpowers" } });
  fireEvent.click(screen.getByRole("button", { name: "Install" }));

  await waitFor(() => expect(beginSkillInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "obra/superpowers"));
  expect(await screen.findByText(/tool\.before\/g\.sh/)).toBeTruthy();
});

test("a curated source completes immediately without a trust step", async () => {
  beginSkillInstall.mockImplementationOnce(() => ok({ completed: true, trust: null, plugin: installedPack }));
  await renderSkillWizard("superpowers");

  await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("Acme Pack installed"));
  expect(listPlugins).toHaveBeenCalled();
  expect(onClose).toHaveBeenCalled();
  expect(screen.queryByText(/tool\.before/)).toBeNull();
});

test("a beginSkillInstall error toasts and falls back to the source-entry step", async () => {
  beginSkillInstall.mockImplementationOnce(() => err("source not found"));
  await renderSkillWizard("does-not-exist");

  await waitFor(() => expect(toastError).toHaveBeenCalledWith("Skill install failed: source not found"));
  expect(await screen.findByLabelText("Skill source")).toBeTruthy();
  expect(onClose).not.toHaveBeenCalled();
});

test("Cancel on the source step closes without calling beginSkillInstall", async () => {
  await renderSkillWizard();

  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalled();
  expect(beginSkillInstall).not.toHaveBeenCalled();
});

test("confirming installs, reloads the plugin list, and closes", async () => {
  await renderSkillWizard("acme/p");
  await screen.findByText(/tool\.before\/g\.sh/);

  fireEvent.click(screen.getByRole("button", { name: "Trust & Install" }));

  await waitFor(() => expect(confirmSkillInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "t"));
  await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("Acme Pack installed"));
  expect(listPlugins).toHaveBeenCalled();
  expect(onClose).toHaveBeenCalled();
});

test("a confirmSkillInstall error toasts and keeps the modal open on the trust step", async () => {
  confirmSkillInstall.mockImplementationOnce(() => err("token expired"));
  await renderSkillWizard("acme/p");
  await screen.findByText(/tool\.before\/g\.sh/);

  fireEvent.click(screen.getByRole("button", { name: "Trust & Install" }));

  await waitFor(() => expect(toastError).toHaveBeenCalledWith("Skill install failed: token expired"));
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByText(/tool\.before\/g\.sh/)).toBeTruthy();
});

test("a trust prompt with no hook scripts renders no hook-script warning", async () => {
  beginSkillInstall.mockImplementationOnce(() =>
    ok({
      completed: false,
      trust: { ...trustPrompt, hookScripts: [] },
      plugin: null,
    }),
  );
  await renderSkillWizard("acme/p");

  await screen.findByText(/acme\/p/);
  expect(screen.queryByText(/Hook scripts/)).toBeNull();
});

test("a trust prompt with runsCode shows a distinct code-execution warning", async () => {
  beginSkillInstall.mockImplementationOnce(() =>
    ok({
      completed: false,
      trust: { ...trustPrompt, runsCode: true },
      plugin: null,
    }),
  );
  await renderSkillWizard("acme/p");

  await screen.findByText(/acme\/p/);
  expect(screen.getByText("Runs code")).toBeTruthy();
  expect(screen.getByText(/runs code in a supervised subprocess/)).toBeTruthy();
});

test("a trust prompt without runsCode renders no code-execution warning", async () => {
  await renderSkillWizard("acme/p");

  await screen.findByText(/acme\/p/);
  expect(screen.queryByText("Runs code")).toBeNull();
  expect(screen.queryByText(/runs code in a supervised subprocess/)).toBeNull();
});

test("a curated-but-code-running trust prompt does not claim the source isn't curated", async () => {
  beginSkillInstall.mockImplementationOnce(() =>
    ok({
      completed: false,
      trust: { ...trustPrompt, runsCode: true, curated: true },
      plugin: null,
    }),
  );
  await renderSkillWizard("superpowers");

  await screen.findByText(/acme\/p/);
  expect(screen.getByText(/is a curated pack, but it runs code/)).toBeTruthy();
  expect(screen.queryByText(/isn't a curated pack/)).toBeNull();
});
