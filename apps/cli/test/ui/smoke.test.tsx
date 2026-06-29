import { test, expect } from "bun:test";
import React from "react";
import { Text } from "ink";
import { render } from "ink-testing-library";

test("ink renders under bun", () => {
  const { lastFrame } = render(<Text>hello-hr</Text>);
  expect(lastFrame()).toContain("hello-hr");
});
