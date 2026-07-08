import { expect, test } from "bun:test";
import { base64ToUtf8, defaultModeForPath, previewImageSrc, previewKindForPath } from "./preview";

test("previewKindForPath classifies the four previewable types", () => {
  expect(previewKindForPath("/x/README.md")).toBe("markdown");
  expect(previewKindForPath("C:\\shots\\a.PNG")).toBe("image");
  expect(previewKindForPath("/x/photo.jpeg")).toBe("image");
  expect(previewKindForPath("/x/anim.gif")).toBe("image");
  expect(previewKindForPath("/x/pic.webp")).toBe("image");
  expect(previewKindForPath("/x/icon.svg")).toBe("svg");
  expect(previewKindForPath("/x/index.html")).toBe("html");
});

test("previewKindForPath returns null for non-previewable files", () => {
  expect(previewKindForPath("/x/main.rs")).toBeNull();
  expect(previewKindForPath("/x/Makefile")).toBeNull();
  expect(previewKindForPath("/x/.md")).toBeNull(); // dotfile, not an extension
});

test("defaultModeForPath is view for previewable files, code otherwise", () => {
  expect(defaultModeForPath("/x/a.md")).toBe("view");
  expect(defaultModeForPath("/x/a.svg")).toBe("view");
  expect(defaultModeForPath("/x/a.ts")).toBe("code");
});

test("previewImageSrc forces the svg mime and falls back when content type is missing", () => {
  expect(previewImageSrc("svg", null, "AA==")).toBe("data:image/svg+xml;base64,AA==");
  expect(previewImageSrc("image", "image/png", "AA==")).toBe("data:image/png;base64,AA==");
  expect(previewImageSrc("image", null, "AA==")).toBe("data:application/octet-stream;base64,AA==");
});

test("base64ToUtf8 decodes multi-byte UTF-8", () => {
  const b64 = Buffer.from("<svg>é✓</svg>", "utf8").toString("base64");
  expect(base64ToUtf8(b64)).toBe("<svg>é✓</svg>");
});
