import type { CatalogEntry, CmdError, ConnectionInfo, Result } from "@/bindings";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { LOCAL_RUNNER } from "@/lib/session-key";

const addConnection = mock((): Promise<Result<ConnectionInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const connectOauth = mock((): Promise<Result<ConnectionInfo[], CmdError>> => new Promise(() => {}));
const listRuntimes = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
const listSelectableModels = mock(() => Promise.resolve({ status: "ok" as const, data: [] }));
const listAgents = mock(() =>
  Promise.resolve({
    status: "ok" as const,
    data: { agents: [], defaultAgentId: "", recovery: [], subagentModel: { kind: "route" as const, route: "free" } },
  }),
);
// refreshModelConfiguration() (fired after every successful account mutation)
// also re-fetches runtime info for any project already tracked in
// `projectRuntimeById` — stubbed so a leftover entry from state that outlives
// a single test file doesn't throw on an unmocked call.
const projectRuntimeInfo = mock(() => Promise.resolve({ status: "ok" as const, data: null }));
let oauthAuthorizeUrlListener: ((event: { payload: { provider: string; authorizeUrl: string } }) => void) | null = null;
const listenOauthAuthorizeUrl = mock((cb: (event: { payload: { provider: string; authorizeUrl: string } }) => void) => {
  oauthAuthorizeUrlListener = cb;
  return Promise.resolve(() => {
    if (oauthAuthorizeUrlListener === cb) oauthAuthorizeUrlListener = null;
  });
});

mock.module("@/bindings", () => ({
  commands: { addConnection, connectOauth, listRuntimes, listSelectableModels, listAgents, projectRuntimeInfo },
  events: { oauthAuthorizeUrlMsg: { listen: listenOauthAuthorizeUrl } },
}));

const { AddConnectionModal } = await import("./AddConnectionModal");
const { useConnections } = await import("@/store-connections");
const { PROVIDER_RISK_NOTICE } = await import("@/constants");

const anthropic: CatalogEntry = {
  id: "anthropic",
  name: "Anthropic",
  family: "anthropic",
  color: "#d97757",
  initial: "A",
  category: "api_key",
  format: "anthropic",
  requiresBaseUrl: false,
  models: ["claude-sonnet-4-5"],
  freeTier: false,
  riskNotice: false,
  usesDeviceGrant: false,
};
const claudeOauth: CatalogEntry = {
  id: "anthropic-oauth",
  name: "Claude Code",
  family: "anthropic",
  color: "#d97757",
  initial: "C",
  category: "oauth",
  format: "anthropic",
  requiresBaseUrl: false,
  models: [],
  freeTier: false,
  riskNotice: false,
  usesDeviceGrant: false,
};
const customOpenAi: CatalogEntry = {
  id: "custom-openai",
  name: "Custom (OpenAI-compatible)",
  family: "custom-openai",
  color: "#8b8b8b",
  initial: "C",
  category: "api_key",
  format: "openai",
  requiresBaseUrl: true,
  models: [],
  freeTier: false,
  riskNotice: false,
  usesDeviceGrant: false,
};
const kiroProvider: CatalogEntry = {
  id: "kiro",
  name: "Kiro",
  family: "kiro",
  color: "#7c3aed",
  initial: "K",
  category: "device",
  format: "openai",
  requiresBaseUrl: false,
  models: [],
  freeTier: false,
  riskNotice: true,
  usesDeviceGrant: false,
};

beforeEach(() => {
  useConnections.setState({ catalog: [anthropic, claudeOauth, customOpenAi, kiroProvider], connections: [] });
  addConnection.mockClear();
  connectOauth.mockClear();
  listenOauthAuthorizeUrl.mockClear();
  oauthAuthorizeUrlListener = null;
});

afterEach(cleanup);

test("renders nothing while closed", () => {
  render(<AddConnectionModal open={false} onClose={() => {}} family="anthropic" />);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByText("Add account")).toBeNull();
});

test("Cancel closes without adding a connection", () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(addConnection).not.toHaveBeenCalled();
});

test("single-member family (kiro) goes straight to its device form, no chooser step", () => {
  render(<AddConnectionModal open onClose={() => {}} family="kiro" />);
  expect(screen.getByText("Add account")).toBeTruthy();
  expect(screen.queryByRole("radiogroup", { name: /sign-in method/i })).toBeNull();
  expect(screen.getByLabelText("Label")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Sign in with Kiro" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Import from Kiro IDE" })).toBeTruthy();
  expect(screen.queryByLabelText("API Key")).toBeNull();
});

test("anthropic family offers API Key vs Subscription, then the chosen form", () => {
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);
  const group = screen.getByRole("radiogroup", { name: /sign-in method/i });
  expect(within(group).getByRole("radio", { name: /api key/i })).toBeTruthy();
  expect(within(group).getByRole("radio", { name: /subscription/i })).toBeTruthy();
  expect(screen.getByLabelText("API Key", { selector: "input" })).toBeTruthy();
  fireEvent.click(within(group).getByRole("radio", { name: /subscription/i }));
  expect(screen.getByRole("button", { name: /connect with browser/i })).toBeTruthy();
  expect(screen.queryByLabelText("API Key", { selector: "input" })).toBeNull();
});

test("submitting api key for anthropic family calls addConnection with the member id", async () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  fireEvent.change(screen.getByLabelText("API Key", { selector: "input" }), { target: { value: "sk-ant-test" } });
  fireEvent.click(screen.getByRole("button", { name: "Add account" }));
  await screen.findByText("Add account");
  expect(addConnection).toHaveBeenCalledWith(LOCAL_RUNNER, "anthropic", "Anthropic", "sk-ant-test", null);
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("connecting subscription calls connectOauth with the oauth member id and tracks the authorize URL", async () => {
  const writeText = mock(() => Promise.resolve());
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);
  const group = screen.getByRole("radiogroup", { name: /sign-in method/i });
  fireEvent.click(within(group).getByRole("radio", { name: /subscription/i }));
  fireEvent.click(screen.getByRole("button", { name: /connect with browser/i }));
  expect(connectOauth).toHaveBeenCalledWith(LOCAL_RUNNER, "anthropic-oauth", "Claude Code");

  const authorizeUrl = "https://claude.ai/oauth/authorize?client_id=test";
  await act(async () => {
    oauthAuthorizeUrlListener?.({ payload: { provider: "anthropic-oauth", authorizeUrl } });
  });
  expect(screen.getByDisplayValue(authorizeUrl)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy login URL" }));
  expect(writeText).toHaveBeenCalledWith(authorizeUrl);
});

test("switching auth method mid-flight clears the latched OAuth waiting state", () => {
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);
  const group = screen.getByRole("radiogroup", { name: /sign-in method/i });
  fireEvent.click(within(group).getByRole("radio", { name: /subscription/i }));
  fireEvent.click(screen.getByRole("button", { name: /connect with browser/i }));
  expect(connectOauth).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: /connect with browser/i })).toBeNull();
  fireEvent.click(within(group).getByRole("radio", { name: /api key/i }));
  expect((screen.getByRole("button", { name: "Add account" }) as HTMLButtonElement).disabled).toBe(false);
});

test("single-member custom-openai family requires a base URL before it can be submitted", async () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} family="custom-openai" />);
  expect(screen.queryByRole("radiogroup", { name: /sign-in method/i })).toBeNull();
  const submit = screen.getByRole("button", { name: "Add account" }) as HTMLButtonElement;
  expect(submit.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Local router" } });
  fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "sk-test-123" } });
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "http://127.0.0.1:4000/v1" } });
  expect(submit.disabled).toBe(false);
  fireEvent.click(submit);
  await screen.findByText("Add account");
  expect(addConnection).toHaveBeenCalledWith(LOCAL_RUNNER, "custom-openai", "Local router", "sk-test-123", "http://127.0.0.1:4000/v1");
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("account methods are Choice Cards without category chips", () => {
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);
  expect(screen.getAllByRole("radio").length).toBeGreaterThan(1);
  const chooser = screen.getByRole("radiogroup", { name: "Sign-in method" });
  expect(within(chooser).queryByText(/^OAuth$/)).toBeNull();
  expect(within(chooser).getAllByText(/^API Key$/)).toHaveLength(1);
  const connect = screen.getByRole("button", { name: /Add account|Connect with browser/ });
  expect(connect.closest('[data-slot="modal-footer"]')).not.toBeNull();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
});

test("closing a pending OAuth flow ignores its late completion", async () => {
  let resolveConnect: ((value: Result<ConnectionInfo[], CmdError>) => void) | undefined;
  connectOauth.mockImplementationOnce(
    () =>
      new Promise<Result<ConnectionInfo[], CmdError>>((resolve) => {
        resolveConnect = resolve;
      }),
  );
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  fireEvent.click(screen.getByRole("radio", { name: /Subscription/ }));
  fireEvent.click(screen.getByRole("button", { name: "Connect with browser" }));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await act(async () => {
    resolveConnect?.({ status: "ok", data: [] });
  });
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
});

test("external close and reopen invalidates a pending OAuth completion", async () => {
  let resolveConnect: ((value: Result<ConnectionInfo[], CmdError>) => void) | undefined;
  connectOauth.mockImplementationOnce(
    () =>
      new Promise<Result<ConnectionInfo[], CmdError>>((resolve) => {
        resolveConnect = resolve;
      }),
  );
  const onClose = mock(() => {});
  const view = render(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  fireEvent.click(screen.getByRole("radio", { name: /Subscription/ }));
  fireEvent.click(screen.getByRole("button", { name: "Connect with browser" }));

  view.rerender(<AddConnectionModal open={false} onClose={onClose} family="anthropic" />);
  view.rerender(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  const currentApiKey = screen.getByLabelText("API Key", { selector: "input" });
  fireEvent.change(currentApiKey, { target: { value: "current-session-key" } });

  await act(async () => {
    resolveConnect?.({ status: "ok", data: [] });
  });

  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(0));
  expect((currentApiKey as HTMLInputElement).value).toBe("current-session-key");
});

test("family transition invalidates a pending OAuth completion", async () => {
  let resolveConnect: ((value: Result<ConnectionInfo[], CmdError>) => void) | undefined;
  connectOauth.mockImplementationOnce(
    () =>
      new Promise<Result<ConnectionInfo[], CmdError>>((resolve) => {
        resolveConnect = resolve;
      }),
  );
  const onClose = mock(() => {});
  const view = render(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  fireEvent.click(screen.getByRole("radio", { name: /Subscription/ }));
  fireEvent.click(screen.getByRole("button", { name: "Connect with browser" }));

  view.rerender(<AddConnectionModal open onClose={onClose} family="custom-openai" />);
  const currentLabel = screen.getByLabelText("Label");
  fireEvent.change(currentLabel, { target: { value: "Current family" } });

  await act(async () => {
    resolveConnect?.({ status: "ok", data: [] });
  });

  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(0));
  expect((currentLabel as HTMLInputElement).value).toBe("Current family");
});

test("unmount invalidates a pending OAuth completion", async () => {
  let resolveConnect: ((value: Result<ConnectionInfo[], CmdError>) => void) | undefined;
  connectOauth.mockImplementationOnce(
    () =>
      new Promise<Result<ConnectionInfo[], CmdError>>((resolve) => {
        resolveConnect = resolve;
      }),
  );
  const onClose = mock(() => {});
  const view = render(<AddConnectionModal open onClose={onClose} family="anthropic" />);
  fireEvent.click(screen.getByRole("radio", { name: /Subscription/ }));
  fireEvent.click(screen.getByRole("button", { name: "Connect with browser" }));
  view.unmount();

  await act(async () => {
    resolveConnect?.({ status: "ok", data: [] });
  });

  expect(onClose).toHaveBeenCalledTimes(0);
});

test("short credential commit locks every dismissal and method switch", async () => {
  let resolveAdd: ((value: Result<ConnectionInfo[], CmdError>) => void) | undefined;
  addConnection.mockImplementationOnce(
    () =>
      new Promise<Result<ConnectionInfo[], CmdError>>((resolve) => {
        resolveAdd = resolve;
      }),
  );
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);
  fireEvent.click(screen.getByRole("button", { name: "Add account" }));
  expect((screen.getByRole("button", { name: "Close" }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement).disabled).toBe(true);
  for (const radio of screen.getAllByRole("radio")) {
    expect(radio.getAttribute("aria-disabled") === "true" || (radio as HTMLButtonElement).disabled).toBe(true);
  }
  await act(async () => {
    resolveAdd?.({ status: "error", error: { message: "rejected" } });
  });
  await waitFor(() => expect((screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement).disabled).toBe(false));
});

test("risk-notice providers show the account-suspension warning", () => {
  useConnections.setState({ catalog: [kiroProvider], connections: [], loaded: true });
  render(<AddConnectionModal open onClose={() => {}} family="kiro" />);
  expect(screen.getByText(PROVIDER_RISK_NOTICE)).toBeTruthy();
});

test("no risk notice for ordinary providers", () => {
  useConnections.setState({ catalog: [customOpenAi], connections: [], loaded: true });
  render(<AddConnectionModal open onClose={() => {}} family="custom-openai" />);
  expect(screen.queryByText(PROVIDER_RISK_NOTICE)).toBeNull();
});
