import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ProviderQuotaInfo } from "@/bindings";

type QuotaCommandMocks = {
  connectionProviderQuota: ReturnType<typeof mock<(...args: [string]) => Promise<unknown>>>;
  resetCodexCredit: ReturnType<typeof mock<(...args: [string]) => Promise<unknown>>>;
};

const shared = globalThis as typeof globalThis & { __quotaCommandMocks?: QuotaCommandMocks };
const quotaCommandMocks = (shared.__quotaCommandMocks ??= {
  connectionProviderQuota: mock<(...args: [string]) => Promise<unknown>>(),
  resetCodexCredit: mock<(...args: [string]) => Promise<unknown>>(),
});
const { connectionProviderQuota, resetCodexCredit } = quotaCommandMocks;

mock.module("@/bindings", () => ({ commands: quotaCommandMocks }));

const { AccountQuotaSummary } = await import("./AccountQuotaSummary");

const quota: ProviderQuotaInfo = {
  provider: "future-oauth",
  plan: "Plus",
  message: "Provider quota unavailable",
  limitReached: false,
  reviewLimitReached: false,
  resetCredits: { availableCount: 2, credits: [] },
  quotas: [
    {
      label: "Primary window",
      used: 120,
      total: 100,
      remaining: -20,
      usedPercentage: 120,
      remainingPercentage: -20,
      resetAt: "2026-07-12T11:00:00Z",
      unlimited: false,
    },
    {
      label: "Code review",
      used: 25,
      total: 100,
      remaining: 75,
      usedPercentage: 25,
      remainingPercentage: 75,
      resetAt: null,
      unlimited: false,
    },
  ],
};

afterEach(() => {
  cleanup();
  connectionProviderQuota.mockReset();
  resetCodexCredit.mockReset();
});

function renderSummary(capability: "claude" | "codex" = "codex") {
  const onRequestReset = mock(() => {});
  render(
    <AccountQuotaSummary connectionId="account-1" accountName="Personal Codex" capability={capability} onRequestReset={onRequestReset} />,
  );
  return onRequestReset;
}

test("renders a compact accessible row per quota window with account identity", async () => {
  connectionProviderQuota.mockResolvedValueOnce({ status: "ok", data: quota }).mockResolvedValueOnce({ status: "ok", data: quota });
  const onRequestReset = renderSummary();

  await screen.findByText("Plus");
  expect(screen.getByText("Provider quota unavailable")).toBeTruthy();
  expect(screen.getByText("2 reset credits available")).toBeTruthy();
  expect(screen.getAllByRole("progressbar")).toHaveLength(2);
  expect(screen.getByRole("progressbar", { name: "Personal Codex Primary window quota" }).getAttribute("aria-valuemin")).toBe("0");
  expect(screen.getByRole("progressbar", { name: "Personal Codex Primary window quota" }).getAttribute("aria-valuemax")).toBe("100");
  expect(screen.getByRole("progressbar", { name: "Personal Codex Primary window quota" }).getAttribute("aria-valuenow")).toBe("100");
  expect(screen.getByRole("progressbar", { name: "Personal Codex Code review quota" }).getAttribute("aria-valuenow")).toBe("25");
  expect(screen.getByText(/Resets Jul 12/)).toBeTruthy();
  expect(screen.getByText("No reset time")).toBeTruthy();

  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Refresh quota for Personal Codex" }));
  });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(2);
  fireEvent.click(screen.getByRole("button", { name: "Reset credit for Personal Codex" }));
  expect(onRequestReset).toHaveBeenCalledWith({ accountName: "Personal Codex", onConfirm: expect.any(Function) });
});

test("shows loading state and quota-unavailable retry state", async () => {
  let resolveLoading!: (value: unknown) => void;
  connectionProviderQuota.mockReturnValueOnce(
    new Promise((resolve) => {
      resolveLoading = resolve;
    }),
  );
  renderSummary("claude");
  expect(screen.getByText("Loading quota…")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Refresh quota for Personal Codex" }).hasAttribute("disabled")).toBe(true);

  await act(async () => {
    resolveLoading({ status: "error", error: { message: "Request failed" } });
  });
  await screen.findByText("Quota unavailable");
  connectionProviderQuota.mockResolvedValueOnce({ status: "error", error: { message: "Request failed" } });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Retry quota for Personal Codex" }));
  });
  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledTimes(2));
  expect(screen.queryByRole("button", { name: "Reset credit for Personal Codex" })).toBeNull();
});
