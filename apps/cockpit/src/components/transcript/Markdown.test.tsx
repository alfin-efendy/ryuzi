import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { FileOpenContext, Markdown } from "./Markdown";

afterEach(cleanup);

test("path-like inline code is clickable when a file-open handler is provided", () => {
  const onOpen = mock((_p: string) => {});
  render(
    <FileOpenContext.Provider value={onOpen}>
      <Markdown text={"see `src/store.ts:42` for details"} />
    </FileOpenContext.Provider>,
  );
  fireEvent.click(screen.getByRole("button", { name: "src/store.ts:42" }));
  expect(onOpen).toHaveBeenCalledWith("src/store.ts");
});

test("non-path inline code and no-context renders stay plain", () => {
  const onOpen = mock((_p: string) => {});
  render(
    <FileOpenContext.Provider value={onOpen}>
      <Markdown text={"run `cargo test` first"} />
    </FileOpenContext.Provider>,
  );
  expect(screen.queryByRole("button")).toBeNull();
  cleanup();
  render(<Markdown text={"see `src/store.ts`"} />);
  expect(screen.queryByRole("button")).toBeNull();
});
