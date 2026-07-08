// Pure media-kind classification + byte helpers for composer attachments.

export type MediaKind = "image" | "video" | "audio" | "file";

const IMAGE_EXT = ["png", "jpg", "jpeg", "gif", "webp"];
const VIDEO_EXT = ["mp4", "webm", "mov", "mkv"];
const AUDIO_EXT = ["mp3", "wav", "ogg", "m4a", "flac"];

export function mediaKindForPath(path: string): MediaKind {
  const dot = path.lastIndexOf(".");
  const ext = dot === -1 ? "" : path.slice(dot + 1).toLowerCase();
  if (IMAGE_EXT.includes(ext)) return "image";
  if (VIDEO_EXT.includes(ext)) return "video";
  if (AUDIO_EXT.includes(ext)) return "audio";
  return "file";
}

export function mediaKindForContentType(contentType: string | null | undefined, path: string): MediaKind {
  if (contentType?.startsWith("image/")) return "image";
  if (contentType?.startsWith("video/")) return "video";
  if (contentType?.startsWith("audio/")) return "audio";
  if (contentType) return "file";
  return mediaKindForPath(path);
}

/** Encode a pasted/dropped File as plain base64 (no data: prefix). */
export async function fileToBase64(file: File): Promise<string> {
  const buf = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < buf.length; i += CHUNK) {
    binary += String.fromCharCode(...buf.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}
