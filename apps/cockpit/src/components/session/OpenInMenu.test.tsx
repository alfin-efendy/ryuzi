import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { OpenTarget } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const targets: OpenTarget[] = [
  { id: "explorer", name: "Explorer" },
  { id: "vscode", name: "VS Code" },
];
const listOpenTargets = mock(() => Promise.resolve(targets));
const openIn = mock(() => Promise.resolve({ status: "ok" as const, data: null }));

mock.module("@/bindings", () => ({
  commands: { listOpenTargets, openIn },
}));

const { OpenInMenu } = await import("./OpenInMenu");

beforeEach(() => {
  listOpenTargets.mockClear();
  openIn.mockClear();
});

afterEach(cleanup);

test("local session: fetches targets and the trigger opens a working menu", async () => {
  render(<OpenInMenu runnerId={LOCAL_RUNNER} sessionPk="s1" />);
  const trigger = await screen.findByRole("button", { name: "Open in…" });
  expect(trigger.hasAttribute("disabled")).toBe(false);
  expect(listOpenTargets).toHaveBeenCalledTimes(1);

  fireEvent.click(trigger);
  expect(await screen.findByText("VS Code")).toBeTruthy();
  fireEvent.click(screen.getByText("VS Code"));
  expect(openIn).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", "vscode");
});

test("remote session: renders a disabled trigger with a tooltip and never fetches targets", async () => {
  render(<OpenInMenu runnerId="gw-1" sessionPk="s1" />);
  // Same accessible name ("Open in…") as the enabled trigger — only the
  // wrapping span carries the "why disabled" reason (see component comment).
  const trigger = screen.getByRole("button", { name: "Open in…" }) as HTMLButtonElement;
  expect(trigger.hasAttribute("disabled")).toBe(true);
  // A disabled Button has pointer-events-none so a title on it never shows on
  // hover — the tooltip must live on the wrapping span instead.
  expect(trigger.parentElement?.getAttribute("title")).toBe("Not available for sessions on a remote runner");
  expect(listOpenTargets).not.toHaveBeenCalled();

  fireEvent.click(trigger);
  expect(screen.queryByText("VS Code")).toBeNull();
});

test("remote session with zero local targets still shows the disabled trigger (not the old empty-hide)", async () => {
  listOpenTargets.mockImplementationOnce(() => Promise.resolve([]));
  render(<OpenInMenu runnerId="gw-1" sessionPk="s1" />);
  expect(screen.getByRole("button", { name: "Open in…" })).toBeTruthy();
});
