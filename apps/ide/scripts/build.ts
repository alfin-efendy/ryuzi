// apps/ide/scripts/build.ts — bundles main, preload (Node targets), and the renderer (HTML).
import { $ } from "bun";

await $`bun build ./src/main/index.ts --target=node --external electron --format=cjs --outdir ./dist/main`;
await $`bun build ./src/preload/index.ts --target=node --external electron --format=cjs --outdir ./dist/preload`;
await $`bun build ./src/renderer/index.html --outdir ./dist/renderer`;
console.log("build: dist/{main,preload,renderer} written");
