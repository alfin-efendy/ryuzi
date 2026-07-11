import type { CatalogEntry, CmdError, ConnectionInfo, DeviceFlowInfo, Result } from "@/bindings";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, mock, test } from "bun:test";
import { usesDeviceSignin } from "./deviceSignin";

const info: DeviceFlowInfo = {
  flowId: "flow-qwen",
  userCode: "ABCD-EFGH",
  verificationUri: "https://chat.qwen.ai/device",
  verificationUriComplete: "https://chat.qwen.ai/device?code=ABCD-EFGH",
  expiresIn: 600,
  interval: 5,
};
const startDeviceFlow = mock((): Promise<Result<DeviceFlowInfo, CmdError>> => Promise.resolve({ status: "ok", data: info }));
const awaitDeviceFlow = mock((): Promise<Result<ConnectionInfo[], CmdError>> => new Promise(() => {}));
const connectOauth = mock((): Promise<Result<ConnectionInfo[], CmdError>> => new Promise(() => {}));

mock.module("@/bindings", () => ({
  commands: { startDeviceFlow, awaitDeviceFlow, connectOauth },
  events: { oauthAuthorizeUrlMsg: { listen: mock(() => Promise.resolve(() => {})) } },
}));

const { AddConnectionModal } = await import("./AddConnectionModal");
const { useConnections } = await import("@/store-connections");

const qwenApi: CatalogEntry = {
  id: "qwen-api",
  name: "Qwen API",
  family: "qwen",
  color: "#615CED",
  initial: "Q",
  category: "api_key",
  format: "openai",
  requiresBaseUrl: false,
  models: [],
  freeTier: false,
  riskNotice: false,
  usesDeviceGrant: false,
};
const qwenDevice: CatalogEntry = { ...qwenApi, id: "qwen", name: "Qwen Code", category: "oauth", usesDeviceGrant: true };

afterEach(cleanup);

test("oauth-category device grant renders and starts the device flow", async () => {
  useConnections.setState({ catalog: [qwenApi, qwenDevice], connections: [], loaded: true });
  render(<AddConnectionModal open onClose={() => {}} family="qwen" />);
  fireEvent.click(screen.getByRole("radio", { name: /Device sign-in/ }));
  expect(screen.getByText("Free — sign in with your Qwen account. No API key needed.")).toBeTruthy();
  expect(screen.queryByText(/Waiting for your browser/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
  await waitFor(() => expect(startDeviceFlow).toHaveBeenCalledWith("qwen"));
  await waitFor(() => expect(awaitDeviceFlow).toHaveBeenCalledWith("qwen", "Qwen Code", "flow-qwen"));
  expect(connectOauth).not.toHaveBeenCalled();
});

test("device-category provider uses device sign-in", () => {
  expect(usesDeviceSignin({ ...qwenApi, id: "kiro", category: "device" })).toBe(true);
});

test("device-grant oauth provider uses device sign-in", () => {
  expect(usesDeviceSignin(qwenDevice)).toBe(true);
});

test("redirect oauth provider does not use device sign-in", () => {
  expect(usesDeviceSignin({ ...qwenApi, id: "anthropic-oauth", category: "oauth" })).toBe(false);
});

test("api-key provider does not use device sign-in", () => {
  expect(usesDeviceSignin(qwenApi)).toBe(false);
});
