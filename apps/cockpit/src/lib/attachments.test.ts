import { expect, test } from "bun:test";
import { mediaKindForContentType, mediaKindForPath } from "./attachments";

test("media kind by extension", () => {
  expect(mediaKindForPath("C:\\shots\\a.PNG")).toBe("image");
  expect(mediaKindForPath("/x/clip.mp4")).toBe("video");
  expect(mediaKindForPath("/x/voice.m4a")).toBe("audio");
  expect(mediaKindForPath("/x/notes.pdf")).toBe("file");
  expect(mediaKindForPath("no-extension")).toBe("file");
});

test("media kind prefers content type, falls back to extension", () => {
  expect(mediaKindForContentType("image/webp", "weird.bin")).toBe("image");
  expect(mediaKindForContentType("video/mp4", "weird.bin")).toBe("video");
  expect(mediaKindForContentType("audio/ogg", "weird.bin")).toBe("audio");
  expect(mediaKindForContentType(null, "photo.jpg")).toBe("image");
  expect(mediaKindForContentType("application/zip", "a.zip")).toBe("file");
});
