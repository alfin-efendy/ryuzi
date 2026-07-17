import { afterEach, expect, test } from "bun:test";
import { cleanup, render } from "@testing-library/react";
import type { UsagePoint } from "@/bindings";
import { UsageChart } from "./UsageChart";

afterEach(cleanup);

function pt(day: string, input: number, output: number): UsagePoint {
  return { day, requests: 1, inputTokens: input, outputTokens: output };
}

test("shows the empty state when there are no points", () => {
  const { getByText } = render(<UsageChart points={[]} />);
  expect(getByText("No usage recorded yet.")).toBeTruthy();
});

test("gives each day column a definite height so bars are visible", () => {
  const points = [pt("2026-07-14", 0, 0), pt("2026-07-15", 500, 500), pt("2026-07-16", 100, 100)];
  const { container } = render(<UsageChart points={points} />);

  // Regression guard: without a definite (h-full) column height, the
  // percentage-height bars collapse to ~0px and the chart looks empty.
  const columns = container.querySelectorAll(".h-full");
  expect(columns.length).toBe(points.length);

  // total per day = [0, 1000, 200]; max = 1000 → heights [2% (floor), 100%, 20%].
  const bars = Array.from(container.querySelectorAll<HTMLElement>(".rounded-sm"));
  expect(bars.map((b) => b.style.height)).toEqual(["2%", "100%", "20%"]);
});
