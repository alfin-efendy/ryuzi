import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const { ModelListEditor } = await import("./ModelListEditor");

afterEach(cleanup);

test("adds trimmed unique models without rendering a textarea", () => {
  const onChange = mock((models: string[]) => models);
  const { container } = render(<ModelListEditor models={["gpt-5.2"]} testingModel={null} onChange={onChange} onTestModel={() => {}} />);

  expect(container.querySelector("textarea")).toBeNull();

  fireEvent.change(screen.getByPlaceholderText("Add model id"), { target: { value: "  gpt-5.2-mini  " } });
  fireEvent.click(screen.getByRole("button", { name: "Add model" }));

  expect(onChange).toHaveBeenCalledWith(["gpt-5.2", "gpt-5.2-mini"]);
});

test("removes models and runs per-model tests", () => {
  const onChange = mock((models: string[]) => models);
  const onTestModel = mock((model: string) => model);

  render(
    <ModelListEditor models={["claude-sonnet-4", "claude-opus-4"]} testingModel={null} onChange={onChange} onTestModel={onTestModel} />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Test claude-opus-4" }));
  expect(onTestModel).toHaveBeenCalledWith("claude-opus-4");

  fireEvent.click(screen.getByRole("button", { name: "Remove claude-sonnet-4" }));
  expect(onChange).toHaveBeenCalledWith(["claude-opus-4"]);
});
