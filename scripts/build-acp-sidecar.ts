/**
 * Build ACP sidecar artifacts from the `@agentclientprotocol/claude-agent-acp`
 * npm package: the Tauri external-binary consumed by Cockpit, and (new) a
 * universal JS bundle + standalone binaries + manifest consumed by the Rust
 * CLI's sidecar resolver (Spec 4 §4, `ryuzi_core::sidecar`).
 *
 * Tauri v2 `bundle.externalBin` convention (legacy / no-flag mode):
 *   - Config entry: "binaries/claude-agent-acp"
 *   - On-disk name:  "binaries/claude-agent-acp-<rustc-target-triple>[.exe]"
 *
 * The npm package `@agentclientprotocol/claude-agent-acp` ships a Node/Bun
 * entry-point.  We compile it to a self-contained binary with `bun build
 * --compile` so the bundled app has zero runtime Node/Bun dependency, or —
 * for the CLI sidecar pipeline — bundle it as plain JS (`bun build
 * --target=bun`) to be run with a system `bun` the user already has.
 *
 * Usage (from workspace root):
 *   bun run scripts/build-acp-sidecar.ts [--target <bun-target>]
 *   bun scripts/build-acp-sidecar.ts --bundle --manifest dist/sidecar/manifest.json [--all-targets] [--install-cache]
 *
 * --target is a Bun cross-compile target, e.g.:
 *   bun-linux-x64, bun-linux-arm64,
 *   bun-darwin-x64, bun-darwin-arm64,
 *   bun-windows-x64
 *
 * When --target is omitted (legacy mode) the host target is used (most common
 * for local dev / Cockpit CI matrix builds).
 *
 * New flags (CLI sidecar pipeline — providing any of these switches the
 * script into the new pipeline and skips the legacy host-binary compile):
 *   --bundle                 emit the universal JS bundle (no --compile) to
 *                             dist/sidecar/claude-agent-acp-<ver>.js
 *   --all-targets             compile standalone binaries for every supported
 *                             triple into dist/sidecar/
 *   --manifest <path>         write a sidecar manifest JSON (Spec 4 §4)
 *                             describing the bundle + standalone outputs;
 *                             requires --bundle (the manifest always
 *                             describes the universal bundle's sha256)
 *   --install-cache           copy the bundle into
 *                             ~/.local/share/ryuzi/sidecars/acp/<ver>/adapter.js
 *                             (local dev convenience; requires --bundle)
 *
 * PREREQUISITES (handled here):
 *   1. The adapter is installed into an ISOLATED build dir (not the workspace)
 *      so the workspace bun.lock is never mutated (CI --frozen-lockfile stays valid).
 *   2. bun build --compile / --target=bun ... (produces the binary/bundle)
 *   3. Legacy mode renames with target-triple suffix so Tauri finds it.
 *
 * HUMAN/CI STEPS — see spec3b-task-5-report.md for exact commands.
 *
 * Idempotent: re-running overwrites existing outputs of the same name.
 *
 * NOTE: `spawnSync(file, args, { shell: false })` and `node:fs` `readFileSync`
 * are deliberate (CodeQL js/shell-command-injection-from-environment, and a
 * Bun Blob quirk where `Bun.file().toString()` returns "[object Blob]"
 * instead of file contents) — do not "modernize" these away.
 */

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// This script lives in <repo>/scripts/ — one level up is the repo root.
const REPO_ROOT = resolve(__dirname, "..");
const cockpitDir = join(REPO_ROOT, "apps", "cockpit");
const tauriDir = join(cockpitDir, "src-tauri");
const binariesDir = join(tauriDir, "binaries");

/** Output root for the CLI sidecar pipeline (bundle, standalone binaries, manifest). */
const DIST = join(REPO_ROOT, "dist", "sidecar");

/**
 * Isolated build dir — lives INSIDE src-tauri/ but OUTSIDE the workspace package
 * graph (no parent package.json refers to it).  This ensures `bun add` here does
 * NOT touch the workspace root bun.lock, keeping `bun install --frozen-lockfile`
 * valid in CI.  The directory is gitignored (see src-tauri/.gitignore).
 */
const sidecarBuildDir = join(tauriDir, ".sidecar-build");

/** The npm package that provides the adapter. Pin the version here. */
const ACP_PACKAGE = "@agentclientprotocol/claude-agent-acp";
const ACP_VERSION = "0.55.0";
const ACP_PACKAGE_VERSIONED = `${ACP_PACKAGE}@${ACP_VERSION}`;

/**
 * The bun version this script (and the emitted manifest) was built/tested
 * with. Keep in sync with ci.yml's / release.yml's `bun-version: 1.3.14`.
 * The Rust resolver refuses to use a cached bundle with an older `bun`.
 */
const MIN_BUN = "1.3.14";

/** Binary name without target-triple suffix (must match tauri.conf.json externalBin entry). */
const BIN_NAME = "claude-agent-acp";

/** Bun cross-compile target -> Rust/Tauri target triple. */
const BUN_TO_TRIPLE: Record<string, string> = {
  "bun-linux-x64": "x86_64-unknown-linux-gnu",
  "bun-linux-arm64": "aarch64-unknown-linux-gnu",
  "bun-linux-x64-musl": "x86_64-unknown-linux-musl",
  "bun-linux-arm64-musl": "aarch64-unknown-linux-musl",
  "bun-darwin-x64": "x86_64-apple-darwin",
  "bun-darwin-arm64": "aarch64-apple-darwin",
  "bun-windows-x64": "x86_64-pc-windows-msvc",
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Run a command WITHOUT a shell — arguments are passed as a discrete argv
 * array, so absolute paths (which may contain spaces or shell metacharacters)
 * are never interpreted by a shell. This avoids shell-command-injection from
 * environment-derived paths (CodeQL js/shell-command-injection-from-environment).
 */
function run(file: string, args: string[], opts?: { cwd?: string }): string {
  console.log(`$ ${file} ${args.join(" ")}`);
  const result = spawnSync(file, args, {
    cwd: opts?.cwd ?? REPO_ROOT,
    stdio: ["inherit", "pipe", "inherit"],
    encoding: "utf8",
    shell: false,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Command failed (exit ${result.status}): ${file} ${args.join(" ")}`);
  }
  return (result.stdout ?? "").trim();
}

function sha256File(path: string): string {
  // node:fs readFileSync (not Bun.file()) is deliberate — Bun.file().toString()
  // returns "[object Blob]", not the file's bytes.
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** Derive the Rust/Tauri target triple for the host (or for a Bun target). */
function resolveTargetTriple(bunTarget?: string): string {
  if (bunTarget) {
    const triple = BUN_TO_TRIPLE[bunTarget];
    if (!triple) {
      throw new Error(`Unknown Bun target '${bunTarget}'. Supported: ${Object.keys(BUN_TO_TRIPLE).join(", ")}`);
    }
    return triple;
  }

  // Host triple via rustc
  const result = spawnSync("rustc", ["--print", "host-tuple"], {
    encoding: "utf8",
  });
  if (result.status !== 0) {
    // Fallback: derive from process.arch/platform
    const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : process.arch;
    const os =
      process.platform === "linux"
        ? "unknown-linux-gnu"
        : process.platform === "darwin"
          ? "apple-darwin"
          : process.platform === "win32"
            ? "pc-windows-msvc"
            : process.platform;
    console.warn(`[warn] rustc not found; inferring target triple as ${arch}-${os}`);
    return `${arch}-${os}`;
  }
  return result.stdout.trim();
}

/**
 * Ensure the isolated build dir exists with its own package.json and lockfile,
 * then install the adapter package there.  The workspace bun.lock is NOT touched.
 */
function ensureIsolatedInstall(): void {
  mkdirSync(sidecarBuildDir, { recursive: true });

  // Write a minimal package.json if one doesn't exist (or is stale).
  const isolatedPkgPath = join(sidecarBuildDir, "package.json");
  const isolatedPkg = {
    name: "ryuzi-acp-sidecar-build",
    version: "0.0.0",
    private: true,
    dependencies: {
      [ACP_PACKAGE]: ACP_VERSION,
    },
  };
  writeFileSync(isolatedPkgPath, JSON.stringify(isolatedPkg, null, 2) + "\n");

  // Install into the isolated dir.  bun install here creates/updates
  // sidecarBuildDir/bun.lock and sidecarBuildDir/node_modules, leaving the
  // workspace root bun.lock completely untouched.
  run("bun", ["install"], { cwd: sidecarBuildDir });
}

/** Resolve the main entry-point of the installed ACP package. */
function resolveAcpEntryPoint(): string {
  const nodeModules = join(sidecarBuildDir, "node_modules", ACP_PACKAGE);
  if (!existsSync(nodeModules)) {
    throw new Error(`Package ${ACP_PACKAGE} not found at ${nodeModules}. ` + `Run: bun install --cwd ${sidecarBuildDir}`);
  }

  // Read package.json synchronously — Bun.file().toString() returns
  // "[object Blob]" (a Blob, not its contents), so we use readFileSync.
  let pkg: Record<string, unknown>;
  try {
    pkg = JSON.parse(readFileSync(join(nodeModules, "package.json"), "utf8")) as Record<string, unknown>;
  } catch {
    pkg = {};
  }

  // bin field: { "claude-agent-acp": "./dist/index.js" } or string
  const bin = pkg.bin;
  if (typeof bin === "string") return resolve(nodeModules, bin);
  if (typeof bin === "object" && bin !== null) {
    const binObj = bin as Record<string, string>;
    const entry = binObj["claude-agent-acp"] ?? binObj[BIN_NAME] ?? Object.values(binObj)[0];
    if (entry) return resolve(nodeModules, entry);
  }

  // Fallback to main
  const main = (pkg.main as string | undefined) ?? "index.js";
  return resolve(nodeModules, main);
}

/** Universal bundle: plain bun build (no runtime embedded) → a few MB. */
function buildBundle(entry: string): string {
  const out = join(DIST, `${BIN_NAME}-${ACP_VERSION}.js`);
  run("bun", ["build", entry, "--target=bun", "--outfile", out]);
  return out;
}

/** Write the sidecar manifest JSON (Spec 4 §4) matching `SidecarManifest`. */
function writeManifest(path: string, bundlePath: string, binaries: Record<string, string>): void {
  const manifest = {
    version: ACP_VERSION,
    min_bun: MIN_BUN,
    bundle: { asset: basename(bundlePath), sha256: sha256File(bundlePath) },
    standalone: Object.fromEntries(Object.entries(binaries).map(([triple, p]) => [triple, { asset: basename(p), sha256: sha256File(p) }])),
  };
  mkdirSync(dirname(resolve(path)), { recursive: true });
  writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
}

/** Cross-platform local data dir, mirroring Rust `dirs::data_dir()`. */
function localDataDir(): string {
  const home = process.env.HOME ?? process.env.USERPROFILE ?? "";
  if (process.platform === "darwin") {
    return join(home, "Library", "Application Support");
  }
  if (process.platform === "win32") {
    return process.env.APPDATA ?? join(home, "AppData", "Roaming");
  }
  return process.env.XDG_DATA_HOME ?? join(home, ".local", "share");
}

/** Local-dev convenience: seed the resolver's cache so `resolve()` skips the download. */
function installCacheCopy(bundlePath: string): void {
  const dest = join(localDataDir(), "ryuzi", "sidecars", "acp", ACP_VERSION, "adapter.js");
  mkdirSync(dirname(dest), { recursive: true });
  writeFileSync(dest, readFileSync(bundlePath));
  console.log(`Installed dev cache: ${dest}`);
}

/** Legacy / Cockpit mode: compile a self-contained binary into src-tauri/binaries/. */
function buildHostBinary(entryPoint: string, bunTarget: string | undefined): void {
  mkdirSync(binariesDir, { recursive: true });

  const targetTriple = resolveTargetTriple(bunTarget);
  const ext = process.platform === "win32" || bunTarget?.includes("windows") ? ".exe" : "";
  const finalBin = join(binariesDir, `${BIN_NAME}-${targetTriple}${ext}`);
  const tmpBin = join(binariesDir, `${BIN_NAME}${ext}`);

  console.log(`\n--- Step 3: bun build --compile ---`);
  console.log(`Target triple : ${targetTriple}`);
  console.log(`Output (final): ${finalBin}`);

  // Build flags
  const buildFlags = ["build", "--compile", "--minify", `--outfile=${tmpBin}`];
  if (bunTarget) buildFlags.push(`--target=${bunTarget}`);
  buildFlags.push(entryPoint);

  run("bun", buildFlags, { cwd: cockpitDir });

  // Rename to include target triple (Tauri convention)
  if (existsSync(finalBin)) rmSync(finalBin);
  renameSync(tmpBin, finalBin);

  console.log(`\n=== Done: ${finalBin} ===`);
  console.log("Next: bun run --cwd apps/cockpit tauri build  (or tauri dev for smoke test)");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const args = process.argv.slice(2);
function flagValue(name: string): string | undefined {
  const idx = args.indexOf(name);
  return idx >= 0 && idx + 1 < args.length ? args[idx + 1] : undefined;
}
function hasFlag(name: string): boolean {
  return args.includes(name);
}

const bunTarget = flagValue("--target");
const wantBundle = hasFlag("--bundle");
const wantAllTargets = hasFlag("--all-targets");
const manifestPath = flagValue("--manifest");
const wantInstallCache = hasFlag("--install-cache");
const usingSidecarPipeline = wantBundle || wantAllTargets || manifestPath !== undefined;

console.log("=== build-acp-sidecar ===");
console.log(`Package       : ${ACP_PACKAGE_VERSIONED}`);
console.log(`Repo root     : ${REPO_ROOT}`);
console.log(`Isolated build: ${sidecarBuildDir}`);
console.log(`Mode          : ${usingSidecarPipeline ? "sidecar pipeline (bundle/standalone/manifest)" : "legacy (Cockpit host binary)"}`);

// 1. Install the package into the isolated build dir (does NOT touch workspace bun.lock)
console.log("\n--- Step 1: isolated install ---");
ensureIsolatedInstall();

// 2. Resolve entry-point from isolated node_modules
console.log("\n--- Step 2: resolve entry-point ---");
const entryPoint = resolveAcpEntryPoint();
console.log(`Entry: ${entryPoint}`);

if (usingSidecarPipeline) {
  mkdirSync(DIST, { recursive: true });

  let bundlePath: string | undefined;
  if (wantBundle) {
    console.log("\n--- Universal bundle (bun build --target=bun) ---");
    bundlePath = buildBundle(entryPoint);
    console.log(`Bundle: ${bundlePath}`);
  }

  const binaries: Record<string, string> = {};
  if (wantAllTargets) {
    console.log("\n--- Standalone binaries (all targets) ---");
    for (const [bunTargetKey, triple] of Object.entries(BUN_TO_TRIPLE)) {
      const ext = bunTargetKey.includes("windows") ? ".exe" : "";
      const out = join(DIST, `${BIN_NAME}-${ACP_VERSION}-${triple}${ext}`);
      run("bun", ["build", "--compile", "--minify", `--target=${bunTargetKey}`, `--outfile=${out}`, entryPoint]);
      binaries[triple] = out;
    }
  }

  if (manifestPath !== undefined) {
    if (!bundlePath) {
      throw new Error("--manifest requires --bundle (the manifest always describes the universal bundle's sha256)");
    }
    console.log("\n--- Manifest ---");
    writeManifest(manifestPath, bundlePath, binaries);
    console.log(`Manifest: ${manifestPath}`);
  }

  if (wantInstallCache) {
    if (!bundlePath) {
      throw new Error("--install-cache requires --bundle");
    }
    installCacheCopy(bundlePath);
  }

  console.log("\n=== Done ===");
} else {
  buildHostBinary(entryPoint, bunTarget);
}
