import { test, expect } from "bun:test";
import { mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  materializeAttachments,
  buildManifest,
  parseAllowedExt,
  sanitizeName,
  type AttachmentRef,
} from "../src/core/attachments";

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const ELF = new Uint8Array([0x7f, 0x45, 0x4c, 0x46, 1, 1, 1, 0]);
const TXT = new TextEncoder().encode("just a log line\n");

function fakeFetch(bodies: Record<string, Uint8Array>, counter?: { n: number }) {
  return (async (url: string | URL) => {
    if (counter) counter.n++;
    const key = String(url);
    if (!(key in bodies)) return new Response(null, { status: 404 });
    return new Response(bodies[key]);
  }) as unknown as typeof fetch;
}

const ref = (over: Partial<AttachmentRef>): AttachmentRef => ({
  name: "a.png",
  url: "https://cdn/a",
  contentType: "image/png",
  size: PNG.byteLength,
  ...over,
});

function opts(destDir: string, over: Partial<Parameters<typeof materializeAttachments>[1]> = {}) {
  return { destDir, maxBytes: 1_000_000, maxCount: 10, allowedExt: [], ...over };
}

test("saves a valid file and reports it", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const res = await materializeAttachments([ref({})], opts(dir, { fetchImpl: fakeFetch({ "https://cdn/a": PNG }) }));
  expect(res.saved.length).toBe(1);
  expect(res.skipped.length).toBe(0);
  expect(existsSync(res.saved[0]!.path)).toBe(true);
  expect(res.saved[0]!.size).toBe(PNG.byteLength);
});

test("skips oversize before downloading", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const counter = { n: 0 };
  const res = await materializeAttachments(
    [ref({ size: 999_999 })],
    opts(dir, { maxBytes: 10, fetchImpl: fakeFetch({ "https://cdn/a": PNG }, counter) }),
  );
  expect(res.saved.length).toBe(0);
  expect(res.skipped[0]!.reason).toMatch(/exceeds/);
  expect(counter.n).toBe(0); // never fetched
});

test("enforces max count, extras skipped as too many", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const refs = [ref({ url: "u1" }), ref({ url: "u2" }), ref({ url: "u3" })];
  const res = await materializeAttachments(
    refs,
    opts(dir, { maxCount: 2, fetchImpl: fakeFetch({ u1: PNG, u2: PNG, u3: PNG }) }),
  );
  expect(res.saved.length).toBe(2);
  expect(res.skipped.filter((s) => /too many/.test(s.reason)).length).toBe(1);
});

test("extension allowlist filters by extension", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const res = await materializeAttachments(
    [ref({ name: "doc.txt", url: "u1" }), ref({ name: "img.png", url: "u2" })],
    opts(dir, { allowedExt: ["png"], fetchImpl: fakeFetch({ u1: TXT, u2: PNG }) }),
  );
  expect(res.saved.map((s) => s.name)).toEqual(["img.png"]);
  expect(res.skipped[0]!.reason).toMatch(/extension not allowed/);
});

test("rejects content that contradicts its extension (anti-spoof)", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const res = await materializeAttachments(
    [ref({ name: "evil.png", url: "u1" })],
    opts(dir, { fetchImpl: fakeFetch({ u1: ELF }) }),
  );
  expect(res.saved.length).toBe(0);
  expect(res.skipped[0]!.reason).toMatch(/does not match extension/);
});

test("text/unknown extension passes the MIME check", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const res = await materializeAttachments(
    [ref({ name: "server.log", url: "u1", contentType: "text/plain" })],
    opts(dir, { fetchImpl: fakeFetch({ u1: TXT }) }),
  );
  expect(res.saved.length).toBe(1);
});

test("download failure skips the file, others continue", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const res = await materializeAttachments(
    [ref({ name: "gone.png", url: "missing" }), ref({ name: "ok.png", url: "u2" })],
    opts(dir, { fetchImpl: fakeFetch({ u2: PNG }) }),
  );
  expect(res.saved.map((s) => s.name)).toEqual(["ok.png"]);
  expect(res.skipped[0]!.reason).toMatch(/download failed/);
});

test("sanitizes traversal names and dedupes collisions", async () => {
  const dir = mkdtempSync(join(tmpdir(), "att-"));
  const res = await materializeAttachments(
    [ref({ name: "shot.png", url: "u1" }), ref({ name: "shot.png", url: "u2" }), ref({ name: "../../etc/passwd", url: "u3", contentType: "text/plain" })],
    opts(dir, { fetchImpl: fakeFetch({ u1: PNG, u2: PNG, u3: TXT }) }),
  );
  const bases = res.saved.map((s) => s.path.slice(dir.length + 1));
  expect(bases).toContain("shot.png");
  expect(bases).toContain("shot-1.png");
  expect(bases).toContain("passwd");
  expect(bases.every((b) => !b.includes("/") && !b.includes(".."))).toBe(true);
});

test("buildManifest lists saved paths and skips; empty when nothing", () => {
  expect(buildManifest({ saved: [], skipped: [] })).toBe("");
  const text = buildManifest({
    saved: [{ path: "/x/a.png", name: "a.png", contentType: "image/png", size: 240_000 }],
    skipped: [{ name: "huge.zip", reason: "exceeds 26214400 bytes" }],
  });
  expect(text).toContain("/x/a.png");
  expect(text).toContain("image/png");
  expect(text).toContain("Skipped huge.zip");
  expect(text).toContain("exceeds");
});

test("parseAllowedExt normalizes and sanitizeName strips paths", () => {
  expect(parseAllowedExt("PNG, .jpg ,,")).toEqual(["png", "jpg"]);
  expect(parseAllowedExt(undefined)).toEqual([]);
  expect(sanitizeName("../x.png")).toBe("x.png");
  expect(sanitizeName("a b.png")).toBe("a_b.png");
  expect(sanitizeName("..")).toBe("file");
});
