// apps/ide/scripts/dev.ts — watch-build the three bundles and launch electron.
import { $ } from "bun";

// Initial build
await $`bun run scripts/build.ts`;
// Watch each bundle in the background; relaunch electron on demand.
$`bun build ./src/main/index.ts --target=node --external electron --format=cjs --outdir ./dist/main --watch`.nothrow();
$`bun build ./src/preload/index.ts --target=node --external electron --format=cjs --outdir ./dist/preload --watch`.nothrow();
$`bun build ./src/renderer/index.html --outdir ./dist/renderer --watch`.nothrow();
await $`electron dist/main/index.js`;
