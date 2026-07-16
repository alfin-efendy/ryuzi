import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CatalogEntry } from "@/bindings";
import { AddProviderModal, installableFamilies } from "./AddProviderModal";

function entry(id: string, family = id, over: Partial<CatalogEntry> = {}): CatalogEntry {
  return {
    id,
    name: id.toUpperCase(),
    family,
    color: "#123456",
    initial: id[0]!.toUpperCase(),
    category: "api_key",
    format: "openai",
    requiresBaseUrl: false,
    models: [],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
    ...over,
  };
}

afterEach(cleanup);

test("installableFamilies returns only uninstalled family heads", () => {
  const catalog = [
    entry("anthropic"),
    entry("anthropic-oauth", "anthropic"), // member, not a head
    entry("openai"),
    entry("xai"),
  ];
  const ids = installableFamilies(catalog, ["openai"]).map((o) => o.id);
  expect(ids).toEqual(["anthropic", "xai"]);
  // Excludes the installed head (openai) and the non-head member (anthropic-oauth).
  expect(ids).not.toContain("openai");
  expect(ids).not.toContain("anthropic-oauth");
});

test("installableFamilies is empty when every head is installed", () => {
  const catalog = [entry("anthropic"), entry("openai")];
  expect(installableFamilies(catalog, ["anthropic", "openai"])).toEqual([]);
});

test("clicking Install calls onInstall with the family id and closes on success", async () => {
  const onInstall = mock(async (_family: string) => true);
  const onClose = mock(() => {});
  render(
    <AddProviderModal open onClose={onClose} catalog={[entry("xai", "xai", { name: "xAI" })]} installed={[]} onInstall={onInstall} />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Install xAI" }));

  await waitFor(() => expect(onInstall).toHaveBeenCalledWith("xai"));
  await waitFor(() => expect(onClose).toHaveBeenCalled());
});

test("renders the all-installed empty state when there is nothing to add", () => {
  render(<AddProviderModal open onClose={() => {}} catalog={[entry("anthropic")]} installed={["anthropic"]} onInstall={async () => true} />);
  expect(screen.getByText("Every provider is already installed.")).toBeTruthy();
});

test("does not render when closed", () => {
  const { container } = render(
    <AddProviderModal open={false} onClose={() => {}} catalog={[entry("xai")]} installed={[]} onInstall={async () => true} />,
  );
  expect(container.querySelector('[data-slot="modal"]')).toBeNull();
  expect(screen.queryByText("Add provider")).toBeNull();
});
