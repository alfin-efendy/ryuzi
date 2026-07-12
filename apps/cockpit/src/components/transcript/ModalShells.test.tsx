import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { Row } from "@/lib/transcript";
import { LOCAL_RUNNER } from "@/lib/session-key";

afterEach(() => {
  cleanup();
});

mock.module("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => `asset://${path}`,
  invoke: async () => null,
}));
mock.module("@/bindings", () => ({
  commands: {
    sessionWorkdir: async () => ({ status: "ok", data: "/repo" }),
    revertFile: async () => ({ status: "ok", data: null }),
    gitDiff: async () => ({ status: "ok", data: "" }),
    fetchAttachment: async () => ({ status: "ok", data: { dataBase64: "", contentType: "image/png" } }),
  },
}));

const { FileChangeCards } = await import("./FileChangeCards");
const { Transcript } = await import("./Transcript");

test("file revert confirmation uses the shared shell and footer", async () => {
  render(<FileChangeCards runnerId={LOCAL_RUNNER} sessionPk="s1" cards={[{ path: "/repo/file.txt", kind: "edit" }]} />);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  const dialog = screen.getByRole("dialog", { name: "Revert file.txt?" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
});

test("image preview uses the shared shell with visible X and footer action", async () => {
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
    attachments: [{ name: "shot.png", path: "/tmp/shot.png", contentType: "image/png", size: 10, rel: "s1/shot.png" }],
    toolPath: null,
    toolInput: null,
    toolDurationMs: null,
    toolExitCode: null,
    toolSummary: null,
    toolSubagent: null,
  };
  render(<Transcript runnerId={LOCAL_RUNNER} sessionPk="s1" rows={[row]} agentName="Ryuzi" agentColor="#fff" running={false} />);
  // The image loads asynchronously via `commands.fetchAttachment` (remote-safe
  // attachment fetch) — let that microtask settle before the titled button exists.
  await act(async () => {});
  fireEvent.click(screen.getByTitle("shot.png"));
  const dialog = screen.getByRole("dialog", { name: "Image preview" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Close" })).toHaveLength(2);
});
