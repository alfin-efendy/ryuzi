import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";

const reads: string[] = [];

const WORKDIR = "/w";

const TEXT: Record<string, string> = {
  "readme.md": "# Hello preview\n\nbody text",
  "page.html": "<h1>hi</h1>",
};
const B64: Record<string, string> = {
  "logo.svg": Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>', "utf8").toString("base64"),
  "shot.png": "iVBORw0KGgo=",
};
const CT: Record<string, string | null> = { "logo.svg": null, "shot.png": "image/png" };

const ok = (data: unknown) => Promise.resolve({ status: "ok" as const, data });

// Mock the Tauri IPC boundary before the component resolves "@/bindings".
// Reads take (runnerId, sessionPk, rel) — rel is session-workdir-relative,
// resolved by the component from `path`/`workdir` via toRepoRelative.
mock.module("@/bindings", () => ({
  commands: {
    readFile: (runnerId: string, sessionPk: string, rel: string) => {
      reads.push(`text:${runnerId}:${sessionPk}:${rel}`);
      return ok(TEXT[rel] ?? "");
    },
    readFileBase64: (runnerId: string, sessionPk: string, rel: string) => {
      reads.push(`b64:${runnerId}:${sessionPk}:${rel}`);
      return ok({ dataBase64: B64[rel] ?? "", contentType: CT[rel] ?? null });
    },
  },
}));
mock.module("@tauri-apps/plugin-opener", () => ({ openUrl: () => Promise.resolve() }));

const { FileViewer } = await import("./FileViewer");

beforeEach(() => {
  reads.length = 0;
});
afterEach(cleanup);

test("markdown View mode renders the Markdown component, not CodeMirror", async () => {
  const { container } = render(<FileViewer runnerId="r1" sessionPk="s1" path="/w/readme.md" mode="view" workdir={WORKDIR} />);
  await waitFor(() => expect(screen.getByRole("heading", { level: 1 }).textContent).toBe("Hello preview"));
  expect(container.querySelector(".cm-editor")).toBeNull();
  expect(reads).toEqual(["text:r1:s1:readme.md"]);
});

test("svg View mode renders an <img> data URL from a single readFileBase64", async () => {
  const { container } = render(<FileViewer runnerId="r1" sessionPk="s1" path="/w/logo.svg" mode="view" workdir={WORKDIR} />);
  await waitFor(() => expect(container.querySelector("img")).toBeTruthy());
  expect(container.querySelector("img")?.getAttribute("src")).toStartWith("data:image/svg+xml;base64,");
  expect(reads).toEqual(["b64:r1:s1:logo.svg"]);
});

test("html View mode renders a sandboxed iframe with scripts disabled", async () => {
  const { container } = render(<FileViewer runnerId="r1" sessionPk="s1" path="/w/page.html" mode="view" workdir={WORKDIR} />);
  await waitFor(() => expect(container.querySelector("iframe")).toBeTruthy());
  const frame = container.querySelector("iframe");
  expect(frame?.getAttribute("sandbox")).toBe("");
  expect(frame?.getAttribute("srcdoc")).toContain("<h1>hi</h1>");
});

test("toggling View -> Code never re-reads the file", async () => {
  const { container, rerender } = render(<FileViewer runnerId="r1" sessionPk="s1" path="/w/shot.png" mode="view" workdir={WORKDIR} />);
  await waitFor(() => expect(container.querySelector("img")).toBeTruthy());
  rerender(<FileViewer runnerId="r1" sessionPk="s1" path="/w/shot.png" mode="code" workdir={WORKDIR} />);
  expect(screen.getByText(/Binary image/).textContent).toContain("Binary image");
  expect(reads).toEqual(["b64:r1:s1:shot.png"]);
});

test("a tab outside the session workdir shows a graceful message, not a jail error", async () => {
  render(<FileViewer runnerId="r1" sessionPk="s1" path="/elsewhere/secret.txt" mode="code" workdir={WORKDIR} />);
  await waitFor(() => expect(screen.getByText(/outside the session workdir/i)).toBeTruthy());
  expect(reads).toEqual([]);
});

test("no read is attempted while the session workdir hasn't resolved yet", async () => {
  render(<FileViewer runnerId="r1" sessionPk="s1" path="/w/readme.md" mode="code" workdir={null} />);
  await new Promise((r) => setTimeout(r, 0));
  expect(reads).toEqual([]);
});
