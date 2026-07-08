// File-preview classification for the Files explorer View|Code toggle.
import { basename } from "./paths";

export type ViewMode = "view" | "code";
export type PreviewKind = "markdown" | "image" | "svg" | "html";

const IMAGE_EXT = ["png", "jpg", "jpeg", "gif", "webp"];

function extOf(path: string): string {
  const name = basename(path);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** Preview kind rendered by View mode; null = not previewable (Code only, no toggle). */
export function previewKindForPath(path: string): PreviewKind | null {
  const ext = extOf(path);
  if (ext === "md") return "markdown";
  if (IMAGE_EXT.includes(ext)) return "image";
  if (ext === "svg") return "svg";
  if (ext === "html") return "html";
  return null;
}

/** Previewable files open in View; everything else is Code-only. */
export function defaultModeForPath(path: string): ViewMode {
  return previewKindForPath(path) === null ? "code" : "view";
}

/** Data URL for <img> previews. svg forces its mime — read_file_base64 has no svg mapping. */
export function previewImageSrc(kind: PreviewKind, contentType: string | null, dataBase64: string): string {
  const mime = kind === "svg" ? "image/svg+xml" : (contentType ?? "application/octet-stream");
  return `data:${mime};base64,${dataBase64}`;
}

/** Decode base64 file bytes to UTF-8 text (svg Code mode reuses the single base64 read). */
export function base64ToUtf8(b64: string): string {
  return new TextDecoder().decode(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)));
}
