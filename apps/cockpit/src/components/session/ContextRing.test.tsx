import { afterEach, expect, test } from "bun:test";
import { cleanup, render } from "@testing-library/react";

const { ContextRing } = await import("./ContextRing");

afterEach(cleanup);

test("shows percent used and the full circumference as the dash array", () => {
  const { container, getByText } = render(<ContextRing percentLeft={70} />);
  // 30% used.
  expect(getByText("30%")).toBeTruthy();
  const progress = container.querySelector("circle[data-ring='progress']") as SVGCircleElement;
  const r = Number(progress.getAttribute("r"));
  const circ = 2 * Math.PI * r;
  // 30% used → 70% of the circumference remains as offset.
  const offset = Number(progress.getAttribute("stroke-dashoffset"));
  expect(Math.abs(offset - circ * 0.7)).toBeLessThan(0.5);
});

test("stroke color follows the quota ramp as usage climbs", () => {
  const { container: low } = render(<ContextRing percentLeft={90} />); // 10% used → green
  const { container: hi } = render(<ContextRing percentLeft={5} />); // 95% used → red
  const c = (el: Element) => (el.querySelector("circle[data-ring='progress']") as SVGCircleElement).getAttribute("stroke");
  expect(c(low)).toBe("#22C55E");
  expect(c(hi)).toBe("#EF4444");
});
