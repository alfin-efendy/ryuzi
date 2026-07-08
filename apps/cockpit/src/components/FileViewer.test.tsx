import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";

const reads: string[] = [];

const TEXT: Record<string, string> = {
  "/w/readme.md": "# Hello preview\n\nbody text",
  "/w/page.html": "<h1>hi</h1>",
};
const B64: Record<string, string> = {
  "/w/logo.svg": Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>', "utf8").toString("base64"),
  "/w/shot.png": "iVBORw0KGgo=",
};
const CT: Record<string, string | null> = { "/w/logo.svg": null, "/w/shot.png": "image/png" };

const ok = (data: unknown) => Promise.resolve({ status: "ok" as const, data });

// Mock the Tauri IPC boundary before the component resolves "@/bindings".
mock.module("@/bindings", () => ({
  commands: {
    readFile: (path: string) => {
      reads.push(`text:${path}`);
      return ok(TEXT[path] ?? "");
    },
    readFileBase64: (path: string) => {
      reads.push(`b64:${path}`);
      return ok({ dataBase64: B64[path] ?? "", contentType: CT[path] ?? null });
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
  const { container } = render(<FileViewer path="/w/readme.md" mode="view" />);
  await waitFor(() => expect(screen.getByRole("heading", { level: 1 }).textContent).toBe("Hello preview"));
  expect(container.querySelector(".cm-editor")).toBeNull();
  expect(reads).toEqual(["text:/w/readme.md"]);
});

test("svg View mode renders an <img> data URL from a single readFileBase64", async () => {
  const { container } = render(<FileViewer path="/w/logo.svg" mode="view" />);
  await waitFor(() => expect(container.querySelector("img")).toBeTruthy());
  expect(container.querySelector("img")?.getAttribute("src")).toStartWith("data:image/svg+xml;base64,");
  expect(reads).toEqual(["b64:/w/logo.svg"]);
});

test("html View mode renders a sandboxed iframe with scripts disabled", async () => {
  const { container } = render(<FileViewer path="/w/page.html" mode="view" />);
  await waitFor(() => expect(container.querySelector("iframe")).toBeTruthy());
  const frame = container.querySelector("iframe");
  expect(frame?.getAttribute("sandbox")).toBe("");
  expect(frame?.getAttribute("srcdoc")).toContain("<h1>hi</h1>");
});

test("toggling View -> Code never re-reads the file", async () => {
  const { container, rerender } = render(<FileViewer path="/w/shot.png" mode="view" />);
  await waitFor(() => expect(container.querySelector("img")).toBeTruthy());
  rerender(<FileViewer path="/w/shot.png" mode="code" />);
  expect(screen.getByText(/Binary image/).textContent).toContain("Binary image");
  expect(reads).toEqual(["b64:/w/shot.png"]);
});
