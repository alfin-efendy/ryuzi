import type { CatalogEntry, CmdError, ConnectionInfo, Result } from "@/bindings";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, mock, test } from "bun:test";

// Mock the Tauri IPC boundary before the component (and its connections store)
// resolve "@/bindings"; the store's `add` action is the only command the modal hits.
const addConnection = mock((): Promise<Result<ConnectionInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));

mock.module("@/bindings", () => ({ commands: { addConnection } }));

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

const customEndpoint: CatalogEntry = {
  id: "custom",
  name: "Custom endpoint",
  color: "#8b8b8b",
  initial: "C",
  category: "api_key",
  format: "openai",
  requiresBaseUrl: true,
  models: [],
};

const oauthProvider: CatalogEntry = {
  id: "some-oauth",
  name: "OAuth Provider",
  color: "#3178c6",
  initial: "O",
  category: "oauth",
  format: "openai",
  requiresBaseUrl: false,
  models: [],
};

beforeEach(() => {
  useConnections.setState({ catalog: [anthropic, customEndpoint, oauthProvider], connections: [] });
  addConnection.mockClear();
});

afterEach(cleanup);

test("renders nothing while closed", () => {
  render(<AddConnectionModal open={false} onClose={() => {}} />);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByText("Add connection")).toBeNull();
});

test("picker step lists catalog providers and disables non-API-key entries", () => {
  render(<AddConnectionModal open onClose={() => {}} />);
  expect(screen.getByText("Add connection")).toBeTruthy();
  expect(screen.getByText("Pick a provider to connect with an API key.")).toBeTruthy();

  const enabled = screen.getByRole("button", { name: /Anthropic/ }) as HTMLButtonElement;
  expect(enabled.disabled).toBe(false);

  const comingSoon = screen.getByRole("button", { name: /OAuth Provider/ }) as HTMLButtonElement;
  expect(comingSoon.disabled).toBe(true);
  expect(screen.getByText("Coming soon")).toBeTruthy();
});

test("Cancel closes without adding a connection", () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(addConnection).not.toHaveBeenCalled();
});

test("picking a provider shows the credential form and Back returns to the picker", () => {
  render(<AddConnectionModal open onClose={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: /Anthropic/ }));

  expect(screen.getByLabelText("Label")).toBeTruthy();
  expect(screen.getByLabelText("API key")).toBeTruthy();
  expect(screen.getByLabelText(/Base URL override/)).toBeTruthy();
  expect((screen.getByRole("button", { name: "Add Anthropic" }) as HTMLButtonElement).disabled).toBe(false);

  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  expect(screen.getByText("Pick a provider to connect with an API key.")).toBeTruthy();
});

test("submitting an API key adds the connection and closes the modal", async () => {
  const onClose = mock(() => {});
  render(<AddConnectionModal open onClose={onClose} />);
  fireEvent.click(screen.getByRole("button", { name: /Anthropic/ }));

  fireEvent.change(screen.getByLabelText("API key"), { target: { value: "sk-test-123" } });
  fireEvent.click(screen.getByRole("button", { name: "Add Anthropic" }));

  // Success resets the flow back to the picker step before invoking onClose.
  await screen.findByText("Pick a provider to connect with an API key.");
  expect(addConnection).toHaveBeenCalledTimes(1);
  expect(addConnection).toHaveBeenCalledWith("anthropic", "Anthropic", "sk-test-123", null);
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("a provider requiring a base URL keeps submit disabled until one is entered", () => {
  render(<AddConnectionModal open onClose={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: /Custom endpoint/ }));

  const submit = screen.getByRole("button", { name: "Add Custom endpoint" }) as HTMLButtonElement;
  expect(submit.disabled).toBe(true);

  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://llm.internal/v1" } });
  expect(submit.disabled).toBe(false);
});
