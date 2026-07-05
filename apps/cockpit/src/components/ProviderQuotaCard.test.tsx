import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ProviderQuotaInfo } from "@/bindings";

mock.module("@/bindings", () => ({}));

const { ProviderQuotaCard } = await import("./ProviderQuotaCard");

afterEach(cleanup);

const codexQuota: ProviderQuotaInfo = {
  provider: "openai-oauth",
  plan: "Plus",
  message: null,
  limitReached: false,
  reviewLimitReached: true,
  resetCredits: { availableCount: 2, credits: [] },
  quotas: [
    {
      label: "Codex primary",
      used: 64,
      total: 100,
      remaining: 36,
      usedPercentage: 64,
      remainingPercentage: 36,
      resetAt: "2026-07-05T11:00:00Z",
      unlimited: false,
    },
    {
      label: "Code review",
      used: 91,
      total: 100,
      remaining: 9,
      usedPercentage: 91,
      remainingPercentage: 9,
      resetAt: "2026-07-05T12:00:00Z",
      unlimited: false,
    },
  ],
};

test("renders subscription quota windows and Codex reset control", () => {
  const onRefresh = mock(() => {});
  const onResetCredit = mock(() => {});

  render(
    <ProviderQuotaCard
      provider="openai-oauth"
      quota={codexQuota}
      loading={false}
      resetting={false}
      onRefresh={onRefresh}
      onResetCredit={onResetCredit}
    />,
  );

  expect(screen.getByText("Provider quota")).toBeTruthy();
  expect(screen.getByText("Plus")).toBeTruthy();
  expect(screen.getByText("Codex primary")).toBeTruthy();
  expect(screen.getByText("36% left")).toBeTruthy();
  expect(screen.getByText("Code review")).toBeTruthy();
  expect(screen.getByText("2 available")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Reset credit" }));
  expect(onResetCredit).toHaveBeenCalled();
});

test("hides reset credit control for Claude subscription quotas", () => {
  render(
    <ProviderQuotaCard
      provider="anthropic-oauth"
      quota={{ ...codexQuota, provider: "anthropic-oauth", plan: "Claude Code", resetCredits: null }}
      loading={false}
      resetting={false}
      onRefresh={() => {}}
      onResetCredit={() => {}}
    />,
  );

  expect(screen.getByText("Claude Code")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Reset credit" })).toBeNull();
});

test("keeps Codex reset control available before quota details load", () => {
  const onResetCredit = mock(() => {});

  render(
    <ProviderQuotaCard
      provider="openai-oauth"
      quota={null}
      loading={false}
      resetting={false}
      onRefresh={() => {}}
      onResetCredit={onResetCredit}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Reset credit" }));
  expect(onResetCredit).toHaveBeenCalled();
});
