import { afterEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";

const { CategoryBadge } = await import("./bits");

afterEach(cleanup);

test("renders the label for a known category", () => {
  render(<CategoryBadge category="free" />);

  expect(screen.getByText("Free")).toBeTruthy();
});

test("maps the device auth mechanism to the Free pricing badge", () => {
  render(<CategoryBadge category="device" />);

  expect(screen.getByText("Free")).toBeTruthy();
});

test("renders nothing for an unrecognized category", () => {
  const { container } = render(<CategoryBadge category="totally-unknown" />);

  expect(container.firstChild).toBeNull();
  expect(container.textContent).toBe("");
});
