import type { CatalogEntry, CmdError, ConnectionInfo, Result } from "@/bindings";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
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

// `anthropic` and `anthropic-oauth` share the "anthropic" family — the
// modal offers a chooser between them (API key vs Claude subscription).
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
};

// A single-member family (custom-openai) — used to confirm the base-URL
// requirement still gates submission when there's no chooser step.
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
};

// `kiro` is the one catalog entry with a "device" category — a free provider
// that signs in via AWS SSO-OIDC device-code flow (or an import from an
// already-logged-in Kiro IDE) instead of the plain API-key form. It's the
// only member of its family, so no chooser step should render for it.
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
};

beforeEach(() => {
  useConnections.setState({
    catalog: [anthropic, claudeOauth, customOpenAi, kiroProvider],
    connections: [],
  });
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
  expect(screen.queryByLabelText("API key")).toBeNull();
});

test("anthropic family offers API key vs Claude subscription, then the chosen form", () => {
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);

  const group = screen.getByRole("radiogroup", { name: /sign-in method/i });
  expect(within(group).getByRole("radio", { name: /api key/i })).toBeTruthy();
  expect(within(group).getByRole("radio", { name: /claude subscription/i })).toBeTruthy();

  // default selection = the api_key member → API key form visible.
  expect(screen.getByLabelText("API key", { selector: "input" })).toBeTruthy();

  // switch to subscription → oauth connect button.
  fireEvent.click(within(group).getByRole("radio", { name: /claude subscription/i }));
  expect(screen.getByRole("button", { name: /connect with browser/i })).toBeTruthy();
  expect(screen.queryByLabelText("API key", { selector: "input" })).toBeNull();
});

test("submitting api key for anthropic family calls addConnection with the member id", async () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} family="anthropic" />);

  fireEvent.change(screen.getByLabelText("API key", { selector: "input" }), { target: { value: "sk-ant-test" } });
  fireEvent.click(screen.getByRole("button", { name: "Add account" }));

  await screen.findByText("Add account");
  expect(addConnection).toHaveBeenCalledWith("anthropic", "Anthropic", "sk-ant-test", null);
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("connecting subscription calls connectOauth with the oauth member id and tracks the authorize URL", async () => {
  const writeText = mock(() => Promise.resolve());
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);

  const group = screen.getByRole("radiogroup", { name: /sign-in method/i });
  fireEvent.click(within(group).getByRole("radio", { name: /claude subscription/i }));

  fireEvent.click(screen.getByRole("button", { name: /connect with browser/i }));
  expect(connectOauth).toHaveBeenCalledWith("anthropic-oauth", "Claude Code");

  const authorizeUrl = "https://claude.ai/oauth/authorize?client_id=test";
  await act(async () => {
    oauthAuthorizeUrlListener?.({ payload: { provider: "anthropic-oauth", authorizeUrl } });
  });

  expect(screen.getByDisplayValue(authorizeUrl)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy login URL" }));
  expect(writeText).toHaveBeenCalledWith(authorizeUrl);
});

test("switching auth method mid-flight clears the latched OAuth waiting state", async () => {
  render(<AddConnectionModal open onClose={() => {}} family="anthropic" />);

  const group = screen.getByRole("radiogroup", { name: /sign-in method/i });

  // Pick the subscription (oauth) member and start a connect that never
  // resolves (connectOauth is mocked as a never-resolving promise), latching
  // `saving` + `oauthWaiting`.
  fireEvent.click(within(group).getByRole("radio", { name: /claude subscription/i }));
  fireEvent.click(screen.getByRole("button", { name: /connect with browser/i }));
  expect(connectOauth).toHaveBeenCalledTimes(1);
  // The waiting UI has replaced the connect button.
  expect(screen.queryByRole("button", { name: /connect with browser/i })).toBeNull();

  // Switch back to the API-key member mid-flight — this must reset the
  // in-flight state so the form isn't dead.
  fireEvent.click(within(group).getByRole("radio", { name: /api key/i }));

  const submit = screen.getByRole("button", { name: "Add account" }) as HTMLButtonElement;
  expect(submit.disabled).toBe(false);
});

test("single-member custom-openai family requires a base URL before it can be submitted", async () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} family="custom-openai" />);

  expect(screen.queryByRole("radiogroup", { name: /sign-in method/i })).toBeNull();

  const submit = screen.getByRole("button", { name: "Add account" }) as HTMLButtonElement;
  expect(submit.disabled).toBe(true);

  fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Local router" } });
  fireEvent.change(screen.getByLabelText("API key"), { target: { value: "sk-test-123" } });
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "http://127.0.0.1:4000/v1" } });
  expect(submit.disabled).toBe(false);

  fireEvent.click(submit);
  await screen.findByText("Add account");
  expect(addConnection).toHaveBeenCalledWith("custom-openai", "Local router", "sk-test-123", "http://127.0.0.1:4000/v1");
  expect(onClose).toHaveBeenCalledTimes(1);
});
