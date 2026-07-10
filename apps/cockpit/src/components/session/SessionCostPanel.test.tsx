import { afterEach, beforeEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useStore } from "@/store";

const { SessionCostPanel } = await import("./SessionCostPanel");

afterEach(cleanup);
beforeEach(() => {
  useStore.setState({
    contextUsage: {
      s1: { activeTokens: 1000, usableWindow: 190000, percentLeft: 95, contextWindow: 200000, cacheReadTokens: 300, outputTokens: 512 },
    },
    sessionCost: {
      s1: {
        totalUsd: 0.1234,
        models: [{ model: "claude-sonnet-4", input: 100, output: 40, cacheRead: 20, cacheCreation: 5, usd: 0.1234 }],
      },
    },
  });
});

test("shows the ring percent and opens a popover with cost rows", () => {
  render(<SessionCostPanel sessionPk="s1" />);
  expect(screen.getByText("5%")).toBeTruthy(); // 100-95 used
  fireEvent.click(screen.getByRole("button", { name: /context/i }));
  expect(screen.getByText("claude-sonnet-4")).toBeTruthy();
  expect(screen.getByText("$0.12")).toBeTruthy();
});

test("sub-cent total renders <$0.01", () => {
  useStore.setState({
    sessionCost: { s1: { totalUsd: 0.004, models: [{ model: "m", input: 1, output: 0, cacheRead: 0, cacheCreation: 0, usd: 0.004 }] } },
  });
  render(<SessionCostPanel sessionPk="s1" />);
  fireEvent.click(screen.getByRole("button", { name: /context/i }));
  expect(screen.getAllByText("<$0.01").length).toBeGreaterThan(0);
});

test("no cost yet renders a dash but still shows the ring", () => {
  useStore.setState({ sessionCost: {} });
  render(<SessionCostPanel sessionPk="s1" />);
  fireEvent.click(screen.getByRole("button", { name: /context/i }));
  expect(screen.getByText("—")).toBeTruthy();
});

test("multiple models render their own rows plus a separate Total row", () => {
  useStore.setState({
    sessionCost: {
      s1: {
        totalUsd: 0.18,
        models: [
          { model: "claude-sonnet-4", input: 100, output: 40, cacheRead: 20, cacheCreation: 5, usd: 0.12 },
          { model: "gpt-4o-mini", input: 200, output: 10, cacheRead: 0, cacheCreation: 0, usd: 0.06 },
        ],
      },
    },
  });
  render(<SessionCostPanel sessionPk="s1" />);
  fireEvent.click(screen.getByRole("button", { name: /context/i }));
  expect(screen.getByText("claude-sonnet-4")).toBeTruthy();
  expect(screen.getByText("gpt-4o-mini")).toBeTruthy();
  expect(screen.getByText("$0.12")).toBeTruthy();
  expect(screen.getByText("$0.06")).toBeTruthy();
  expect(screen.getByText("Total")).toBeTruthy();
  expect(screen.getByText("$0.18")).toBeTruthy();
});
