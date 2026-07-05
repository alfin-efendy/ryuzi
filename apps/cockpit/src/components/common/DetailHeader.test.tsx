import { afterEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";

const { DetailHeader } = await import("./DetailHeader");

afterEach(cleanup);

test("renders a custom title node in the header title slot", () => {
  render(<DetailHeader chip={<span>A</span>} title="Fallback" titleNode={<input aria-label="Connection label" />} sub="Provider" />);

  expect(screen.getByLabelText("Connection label")).toBeTruthy();
  expect(screen.queryByText("Fallback")).toBeNull();
});
