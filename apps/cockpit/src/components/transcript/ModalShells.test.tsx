import { expect, mock, test } from "bun:test";
import { act, fireEvent, render, screen } from "@testing-library/react";
import type { Row } from "@/lib/transcript";

mock.module("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => `asset://${path}`,
  invoke: async () => null,
}));
mock.module("@/bindings", () => ({
  commands: {
    sessionWorkdir: async () => ({ status: "ok", data: "/repo" }),
    revertFile: async () => ({ status: "ok", data: null }),
    gitDiff: async () => ({ status: "ok", data: "" }),
  },
}));

const { FileChangeCards } = await import("./FileChangeCards");
const { Transcript } = await import("./Transcript");

test("file revert confirmation uses the shared shell and footer", async () => {
  render(<FileChangeCards sessionPk="s1" cards={[{ path: "/repo/file.txt", kind: "edit" }]} />);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  const dialog = screen.getByRole("dialog", { name: "Revert file.txt?" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
});

test("image preview uses the shared shell with visible X and footer action", () => {
  const row: Row = {
    seq: 1,
    role: "user",
    blockType: "text",
    text: "image",
    toolCallId: null,
    toolStatus: null,
    toolKind: null,
    toolName: null,
    toolOutput: null,
    createdAt: 1,
    attachments: [{ name: "shot.png", path: "/tmp/shot.png", contentType: "image/png", size: 10 }],
    toolPath: null,
    toolInput: null,
    toolDurationMs: null,
    toolExitCode: null,
    toolSummary: null,
  };
  render(<Transcript sessionPk="s1" rows={[row]} agentName="Ryuzi" agentColor="#fff" running={false} />);
  fireEvent.click(screen.getByTitle("shot.png"));
  const dialog = screen.getByRole("dialog", { name: "Image preview" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Close" })).toHaveLength(2);
});
