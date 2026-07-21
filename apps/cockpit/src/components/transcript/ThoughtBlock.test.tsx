import { afterEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import { ThoughtBlock } from "./ThoughtBlock";

afterEach(cleanup);

test("shows thought content without an expand/collapse control", () => {
  render(<ThoughtBlock markdown="Consider **all** edge cases." streaming={false} />);

  expect(screen.getByText("Thought")).toBeTruthy();
  expect(screen.getByText("all").tagName).toBe("STRONG");
  expect(screen.queryByRole("button")).toBeNull();
});
