import { expect, test } from "bun:test";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { COMPONENTS } from "./build-first-party.ts";

// Repo root relative to this file (scripts/plugins/), so the test is
// cwd-independent — it resolves plugins/ the same way regardless of where
// `bun test` is invoked from.
const REPO_ROOT = join(import.meta.dir, "..", "..");
const PLUGINS_DIR = join(REPO_ROOT, "plugins");

/**
 * Every `plugins/<id>/` that ships a `ryuzi-plugin.toml` is a first-party
 * component the release pipeline MUST build + sign. Sibling `plugins/*` dirs
 * without a manifest (the shared `openai-format` / `anthropic-format` wire
 * crates) are path dependencies, not bundles, so they are excluded here.
 */
async function shippedComponentIds(): Promise<string[]> {
  const entries = await readdir(PLUGINS_DIR, { withFileTypes: true });
  const ids: string[] = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    if (await Bun.file(join(PLUGINS_DIR, entry.name, "ryuzi-plugin.toml")).exists()) {
      ids.push(entry.name);
    }
  }
  return ids.sort();
}

// The drift-guard. `build-first-party.ts`'s COMPONENTS list must cover EVERY
// shipped component. Without this, a new provider component added under
// `plugins/` would silently never be built or signed by the release pipeline —
// the exact gap that shipped `anthropic-oauth` and `qwen` unbuilt. It fails in
// BOTH directions: a missing entry (drift the guard exists to catch) and a
// stale entry whose `plugins/<id>/` manifest was removed.
test("COMPONENTS covers every shipped plugins/<id>/ that has a ryuzi-plugin.toml", async () => {
  const shipped = await shippedComponentIds();
  const listed = COMPONENTS.map((c) => c.id).sort();

  const missing = shipped.filter((id) => !listed.includes(id));
  const stale = listed.filter((id) => !shipped.includes(id));

  expect(missing).toEqual([]); // shipped component the release pipeline would never build/sign
  expect(stale).toEqual([]); // COMPONENTS entry whose plugins/<id>/ manifest no longer exists
});

// Each COMPONENTS entry must point at a real dir whose manifest declares the
// SAME id. `processComponent` already enforces this at build time; asserting it
// here makes a typo fail `bun test`, not a live release.
test("each COMPONENTS entry's dir exists and its manifest id matches", async () => {
  for (const spec of COMPONENTS) {
    const manifest = Bun.file(join(REPO_ROOT, spec.dir, "ryuzi-plugin.toml"));
    expect(await manifest.exists()).toBe(true);
    const parsed = Bun.TOML.parse(await manifest.text()) as { id?: unknown };
    expect(parsed.id).toBe(spec.id);
  }
});

// `crateWasmStem` is cargo's wasm output filename: the crate's `[package] name`
// with `-` -> `_`. A mismatch makes `processComponent` read a nonexistent
// `<stem>.wasm` and the release fails — so pin it to each crate's real name.
test("each COMPONENTS entry's crateWasmStem matches its crate's [package] name", async () => {
  for (const spec of COMPONENTS) {
    const parsed = Bun.TOML.parse(await Bun.file(join(REPO_ROOT, spec.dir, "Cargo.toml")).text()) as {
      package?: { name?: unknown };
    };
    const crateName = parsed.package?.name;
    expect(typeof crateName).toBe("string");
    expect(spec.crateWasmStem).toBe((crateName as string).replaceAll("-", "_"));
  }
});
