/**
 * Seeds apps/cockpit/app-icon.png from the canonical brand mark
 * (assets/brand/mark-solid.png, 1024×1024 — the app-icon variant per
 * assets/brand/README.md), then the Tauri icon set is derived from it.
 *
 * Run: bun scripts/gen-icon.ts && bun run tauri icon ./app-icon.png
 */

import { copyFileSync } from "node:fs";
import { join } from "node:path";

const src = join(import.meta.dir, "..", "..", "..", "assets", "brand", "mark-solid.png");
const out = join(import.meta.dir, "..", "app-icon.png");

copyFileSync(src, out);
console.log(`Written: ${out} (copied from ${src})`);
