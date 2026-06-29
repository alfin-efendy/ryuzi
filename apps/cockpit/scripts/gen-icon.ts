/**
 * Generates a 1024×1024 RGBA PNG (solid indigo #4f46e5) as apps/cockpit/app-icon.png.
 * Used once to seed `bun run tauri icon ./app-icon.png`.
 *
 * Run: bun scripts/gen-icon.ts
 */

import { PNG } from "pngjs";
import { join } from "node:path";
import { writeFileSync } from "node:fs";

const SIZE = 1024;

// Brand colour: indigo #4f46e5
const R = 0x4f;
const G = 0x46;
const B = 0xe5;
const A = 0xff;

const png = new PNG({ width: SIZE, height: SIZE, colorType: 6 }); // RGBA

for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    const idx = (SIZE * y + x) * 4;
    png.data[idx + 0] = R;
    png.data[idx + 1] = G;
    png.data[idx + 2] = B;
    png.data[idx + 3] = A;
  }
}

const outPath = join(import.meta.dir, "..", "app-icon.png");
const buf = PNG.sync.write(png);
writeFileSync(outPath, buf);

console.log(`Written: ${outPath} (${SIZE}x${SIZE} px, ${buf.byteLength} bytes)`);
