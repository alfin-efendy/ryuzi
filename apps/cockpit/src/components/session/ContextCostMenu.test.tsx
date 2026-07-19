import { afterEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import { ContextCostMenu } from "./ContextCostMenu";

afterEach(cleanup);

test("renders Context + Cost sections from props", () => {
  render(
    <ContextCostMenu
      onClose={() => {}}
      usage={{ activeTokens: 100, usableWindow: 900, contextWindow: 1000, cacheReadTokens: 30 }}
      cost={{ totalUsd: 0.5, models: [{ model: "claude-opus-4-8", input: 10, output: 4, cacheRead: 2, cacheCreation: 1, usd: 0.5 }] }}
    />,
  );
  expect(screen.getByText("Context")).toBeTruthy();
  expect(screen.getByText("claude-opus-4-8")).toBeTruthy();
  expect(screen.getByText("Cache reads")).toBeTruthy();
});
