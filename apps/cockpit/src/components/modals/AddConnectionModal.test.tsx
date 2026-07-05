import type { CatalogEntry, CmdError, ConnectionInfo, Result } from "@/bindings";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, mock, test } from "bun:test";

const addConnection = mock((): Promise<Result<ConnectionInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const connectOauth = mock((): Promise<Result<ConnectionInfo[], CmdError>> => new Promise(() => {}));
let oauthAuthorizeUrlListener: ((event: { payload: { provider: string; authorizeUrl: string } }) => void) | null = null;
const listenOauthAuthorizeUrl = mock((cb: (event: { payload: { provider: string; authorizeUrl: string } }) => void) => {
  oauthAuthorizeUrlListener = cb;
  return Promise.resolve(() => {
    if (oauthAuthorizeUrlListener === cb) oauthAuthorizeUrlListener = null;
  });
});

mock.module("@/bindings", () => ({
  commands: { addConnection, connectOauth },
  events: {
    oauthAuthorizeUrlMsg: {
      listen: listenOauthAuthorizeUrl,
    },
  },
}));

const { AddConnectionModal } = await import("./AddConnectionModal");
const { useConnections } = await import("@/store-connections");

const anthropic: CatalogEntry = {
  id: "anthropic",
  name: "Anthropic",
  color: "#d97757",
  initial: "A",
  category: "api_key",
  format: "anthropic",
  requiresBaseUrl: false,
  models: ["claude-sonnet-4-5"],
};

const customOpenAi: CatalogEntry = {
  id: "custom-openai",
  name: "Custom (OpenAI-compatible)",
  color: "#8b8b8b",
  initial: "C",
  category: "api_key",
  format: "openai",
  requiresBaseUrl: true,
  models: [],
};

const customAnthropic: CatalogEntry = {
  id: "custom-anthropic",
  name: "Custom (Anthropic-compatible)",
  color: "#8b8b8b",
  initial: "C",
  category: "api_key",
  format: "anthropic",
  requiresBaseUrl: true,
  models: [],
};

const claudeOauth: CatalogEntry = {
  id: "anthropic-oauth",
  name: "Claude Code",
  color: "#d97757",
  initial: "C",
  category: "oauth",
  format: "anthropic",
  requiresBaseUrl: false,
  models: [],
};

// `kiro` is the one catalog entry with a "device" category — a free provider
// that signs in via AWS SSO-OIDC device-code flow (or an import from an
// already-logged-in Kiro IDE) instead of the plain API-key form.
const kiroProvider: CatalogEntry = {
  id: "kiro",
  name: "Kiro",
  color: "#7c3aed",
  initial: "K",
  category: "device",
  format: "openai",
  requiresBaseUrl: false,
  models: [],
};

beforeEach(() => {
  useConnections.setState({
    catalog: [anthropic, customOpenAi, customAnthropic, claudeOauth, kiroProvider],
    connections: [],
  });
  addConnection.mockClear();
  connectOauth.mockClear();
  listenOauthAuthorizeUrl.mockClear();
  oauthAuthorizeUrlListener = null;
});

afterEach(cleanup);

test("renders nothing while closed", () => {
  render(<AddConnectionModal open={false} onClose={() => {}} />);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByText("Add connection")).toBeNull();
});

test("global add offers only OpenAI-compatible and Anthropic-compatible cards", () => {
  render(<AddConnectionModal open onClose={() => {}} />);
  expect(screen.getByRole("button", { name: "Add connection" })).toBeTruthy();
  expect(screen.getByRole("radio", { name: /OpenAI-compatible/ })).toBeTruthy();
  expect(screen.getByRole("radio", { name: /Anthropic-compatible/ })).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Anthropic$/ })).toBeNull();
  expect(screen.getByLabelText("Label")).toBeTruthy();
  expect(screen.getByLabelText("API key")).toBeTruthy();
  expect(screen.getByLabelText("Base URL")).toBeTruthy();
});

test("Cancel closes without adding a connection", () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(addConnection).not.toHaveBeenCalled();
});

test("submitting the global OpenAI-compatible form adds a custom-openai connection", async () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} />);

  fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Local router" } });
  fireEvent.change(screen.getByLabelText("API key"), { target: { value: "sk-test-123" } });
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "http://127.0.0.1:4000/v1" } });
  fireEvent.click(screen.getByRole("button", { name: "Add connection" }));

  await screen.findByText("Add connection");
  expect(addConnection).toHaveBeenCalledWith("custom-openai", "Local router", "sk-test-123", "http://127.0.0.1:4000/v1");
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("selecting Anthropic-compatible switches the provider id used by submit", async () => {
  render(<AddConnectionModal open onClose={() => {}} />);
  fireEvent.click(screen.getByRole("radio", { name: /Anthropic-compatible/ }));

  fireEvent.change(screen.getByLabelText("API key"), { target: { value: "sk-ant-test" } });
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://llm.internal/v1" } });
  fireEvent.click(screen.getByRole("button", { name: "Add connection" }));

  await screen.findByText("Add connection");
  expect(addConnection).toHaveBeenCalledWith("custom-anthropic", "Custom (Anthropic-compatible)", "sk-ant-test", "https://llm.internal/v1");
});

test("fixed provider add account does not render the compatible picker", async () => {
  render(<AddConnectionModal open onClose={() => {}} provider="anthropic" />);

  expect(screen.getByRole("button", { name: "Add account" })).toBeTruthy();
  expect(screen.queryByRole("radio", { name: /OpenAI-compatible/ })).toBeNull();
  expect(screen.queryByRole("radio", { name: /Anthropic-compatible/ })).toBeNull();
  expect(screen.getByText("Anthropic")).toBeTruthy();

  fireEvent.change(screen.getByLabelText("API key"), { target: { value: "sk-ant-test" } });
  fireEvent.click(screen.getByRole("button", { name: "Add account" }));

  await screen.findByText("Add account");
  expect(addConnection).toHaveBeenCalledWith("anthropic", "Anthropic", "sk-ant-test", null);
});

test("compatible form requires a base URL", () => {
  render(<AddConnectionModal open onClose={() => {}} />);

  const submit = screen.getByRole("button", { name: "Add connection" }) as HTMLButtonElement;
  expect(submit.disabled).toBe(true);

  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://llm.internal/v1" } });
  expect(submit.disabled).toBe(false);
});

test("fixed OAuth provider connects with browser and exposes a copyable login URL", async () => {
  const writeText = mock(() => Promise.resolve());
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  render(<AddConnectionModal open onClose={() => {}} provider="anthropic-oauth" />);

  expect(screen.getByText("Claude Code")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Connect with browser" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Paste code instead" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Connect with browser" }));
  expect(connectOauth).toHaveBeenCalledWith("anthropic-oauth", "Claude Code");

  const authorizeUrl = "https://claude.ai/oauth/authorize?client_id=test";
  await act(async () => {
    oauthAuthorizeUrlListener?.({ payload: { provider: "anthropic-oauth", authorizeUrl } });
  });

  expect(screen.getByDisplayValue(authorizeUrl)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy login URL" }));
  expect(writeText).toHaveBeenCalledWith(authorizeUrl);
});

test("fixed device provider (kiro) shows sign-in and import actions, not an API key form", () => {
  render(<AddConnectionModal open onClose={() => {}} provider="kiro" />);

  expect(screen.getByText("Add account")).toBeTruthy();
  expect(screen.getByLabelText("Label")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Sign in with Kiro" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Import from Kiro IDE" })).toBeTruthy();
  expect(screen.queryByLabelText("API key")).toBeNull();
});
