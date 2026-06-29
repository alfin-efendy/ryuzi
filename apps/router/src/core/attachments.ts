import { mkdirSync } from "node:fs";
import { join } from "node:path";
import type { AttachmentRef } from "@harness/protocol";

export type { AttachmentRef };

export interface MaterializeOpts {
  destDir: string;
  maxBytes: number;
  maxCount: number;
  allowedExt: string[]; // lowercase, no leading dot; [] = allow all
  fetchImpl?: typeof fetch;
}
export interface SavedAttachment {
  path: string;
  name: string;
  contentType?: string;
  size: number;
}
export interface SkippedAttachment {
  name: string;
  reason: string;
}
export interface MaterializeResult {
  saved: SavedAttachment[];
  skipped: SkippedAttachment[];
}

const SIGNATURES: { ext: string[]; magic: number[] }[] = [
  { ext: ["png"], magic: [0x89, 0x50, 0x4e, 0x47] },
  { ext: ["jpg", "jpeg"], magic: [0xff, 0xd8, 0xff] },
  { ext: ["gif"], magic: [0x47, 0x49, 0x46, 0x38] },
  { ext: ["pdf"], magic: [0x25, 0x50, 0x44, 0x46] },
  { ext: ["zip"], magic: [0x50, 0x4b, 0x03, 0x04] },
  { ext: ["gz", "gzip"], magic: [0x1f, 0x8b] },
];

function extOf(name: string): string {
  const m = name.toLowerCase().match(/\.([a-z0-9]+)$/);
  return m ? m[1]! : "";
}

function startsWith(bytes: Uint8Array, magic: number[]): boolean {
  if (bytes.length < magic.length) return false;
  return magic.every((b, i) => bytes[i] === b);
}

// Contradiction-only: if the extension implies a known signature, the bytes must match it.
// Unknown / text extensions are never flagged.
function contradictsExtension(ext: string, bytes: Uint8Array): boolean {
  const expected = SIGNATURES.find((s) => s.ext.includes(ext));
  if (!expected) return false;
  return !startsWith(bytes, expected.magic);
}

export function sanitizeName(name: string): string {
  const base = name.split(/[/\\]/).pop() ?? "";
  const cleaned = base.replace(/[^A-Za-z0-9._-]/g, "_").replace(/^\.+/, "");
  return cleaned || "file";
}

function dedupe(name: string, used: Set<string>): string {
  if (!used.has(name)) {
    used.add(name);
    return name;
  }
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  let i = 1;
  let candidate = `${stem}-${i}${ext}`;
  while (used.has(candidate)) {
    i++;
    candidate = `${stem}-${i}${ext}`;
  }
  used.add(candidate);
  return candidate;
}

export function parseAllowedExt(raw: string | undefined): string[] {
  return (raw ?? "")
    .split(",")
    .map((s) => s.trim().toLowerCase().replace(/^\./, ""))
    .filter(Boolean);
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export async function materializeAttachments(refs: AttachmentRef[], opts: MaterializeOpts): Promise<MaterializeResult> {
  const saved: SavedAttachment[] = [];
  const skipped: SkippedAttachment[] = [];
  const doFetch = opts.fetchImpl ?? fetch;
  const used = new Set<string>();
  let accepted = 0;

  for (const ref of refs) {
    if (accepted >= opts.maxCount) {
      skipped.push({ name: ref.name, reason: "too many attachments" });
      continue;
    }
    if (ref.size > opts.maxBytes) {
      skipped.push({ name: ref.name, reason: `exceeds ${opts.maxBytes} bytes` });
      continue;
    }
    const ext = extOf(ref.name);
    if (opts.allowedExt.length > 0 && !opts.allowedExt.includes(ext)) {
      skipped.push({ name: ref.name, reason: "extension not allowed" });
      continue;
    }

    let bytes: Uint8Array;
    try {
      const res = await doFetch(ref.url);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      bytes = new Uint8Array(await res.arrayBuffer());
    } catch (e) {
      skipped.push({ name: ref.name, reason: `download failed: ${(e as Error).message}` });
      continue;
    }

    if (bytes.byteLength > opts.maxBytes) {
      skipped.push({ name: ref.name, reason: `exceeds ${opts.maxBytes} bytes` });
      continue;
    }
    if (contradictsExtension(ext, bytes)) {
      skipped.push({ name: ref.name, reason: "content does not match extension" });
      continue;
    }

    mkdirSync(opts.destDir, { recursive: true });
    const path = join(opts.destDir, dedupe(sanitizeName(ref.name), used));
    await Bun.write(path, bytes);
    saved.push({ path, name: ref.name, contentType: ref.contentType, size: bytes.byteLength });
    accepted++;
  }

  return { saved, skipped };
}

export function buildManifest(result: MaterializeResult): string {
  const lines: string[] = [];
  if (result.saved.length > 0) {
    const n = result.saved.length;
    lines.push(`[User attached ${n} file${n > 1 ? "s" : ""} — saved to disk, use the Read tool to open them:]`);
    for (const f of result.saved) {
      lines.push(`- ${f.path} (${f.contentType ?? "unknown"}, ${formatBytes(f.size)})`);
    }
  }
  for (const s of result.skipped) {
    lines.push(`⚠️ Skipped ${s.name}: ${s.reason}`);
  }
  return lines.join("\n");
}
