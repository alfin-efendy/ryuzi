/**
 * Build the claude-agent-acp sidecar binary and place it under
 * src-tauri/binaries/ with the Tauri-required target-triple suffix.
 *
 * Tauri v2 `bundle.externalBin` convention:
 *   - Config entry: "binaries/claude-agent-acp"
 *   - On-disk name:  "binaries/claude-agent-acp-<rustc-target-triple>[.exe]"
 *
 * The npm package `@agentclientprotocol/claude-agent-acp` ships a Node/Bun
 * entry-point.  We compile it to a self-contained binary with `bun build
 * --compile` so the bundled app has zero runtime Node/Bun dependency.
 *
 * Usage (from workspace root):
 *   bun run apps/cockpit/scripts/build-acp-sidecar.ts [--target <bun-target>]
 *
 * Or from apps/cockpit/:
 *   bun scripts/build-acp-sidecar.ts
 *
 * --target is a Bun cross-compile target, e.g.:
 *   bun-linux-x64, bun-linux-arm64,
 *   bun-darwin-x64, bun-darwin-arm64,
 *   bun-windows-x64
 *
 * When --target is omitted the host target is used (most common for local dev).
 *
 * PREREQUISITES (handled here):
 *   1. The adapter is installed into an ISOLATED build dir (not the workspace)
 *      so the workspace bun.lock is never mutated (CI --frozen-lockfile stays valid).
 *   2. bun build --compile ... (produces the binary)
 *   3. Rename with target-triple suffix so Tauri finds it.
 *
 * HUMAN/CI STEPS — see spec3b-task-5-report.md for exact commands.
 *
 * Idempotent: re-running overwrites an existing binary of the same name.
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// This script lives in apps/cockpit/scripts/ — two levels up is apps/cockpit/
const cockpitDir = resolve(__dirname, "..");
const tauriDir = join(cockpitDir, "src-tauri");
const binariesDir = join(tauriDir, "binaries");

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

/** Binary name without target-triple suffix (must match tauri.conf.json externalBin entry). */
const BIN_NAME = "claude-agent-acp";

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
    cwd: opts?.cwd ?? cockpitDir,
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

/** Derive the Rust/Tauri target triple for the host (or for a Bun target). */
function resolveTargetTriple(bunTarget?: string): string {
  if (bunTarget) {
    // Map Bun cross-compile targets to Rust target triples.
    const map: Record<string, string> = {
      "bun-linux-x64": "x86_64-unknown-linux-gnu",
      "bun-linux-x64-musl": "x86_64-unknown-linux-musl",
      "bun-linux-arm64": "aarch64-unknown-linux-gnu",
      "bun-linux-arm64-musl": "aarch64-unknown-linux-musl",
      "bun-darwin-x64": "x86_64-apple-darwin",
      "bun-darwin-arm64": "aarch64-apple-darwin",
      "bun-windows-x64": "x86_64-pc-windows-msvc",
    };
    const triple = map[bunTarget];
    if (!triple) {
      throw new Error(`Unknown Bun target '${bunTarget}'. Supported: ${Object.keys(map).join(", ")}`);
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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const args = process.argv.slice(2);
const targetIdx = args.indexOf("--target");
const bunTarget = targetIdx >= 0 ? args[targetIdx + 1] : undefined;

console.log("=== build-acp-sidecar ===");
console.log(`Package       : ${ACP_PACKAGE_VERSIONED}`);
console.log(`Cockpit       : ${cockpitDir}`);
console.log(`Isolated build: ${sidecarBuildDir}`);
console.log(`Binaries      : ${binariesDir}`);

// 1. Ensure binaries/ exists
mkdirSync(binariesDir, { recursive: true });

// 2. Install the package into the isolated build dir (does NOT touch workspace bun.lock)
console.log("\n--- Step 1: isolated install ---");
ensureIsolatedInstall();

// 3. Resolve entry-point from isolated node_modules
console.log("\n--- Step 2: resolve entry-point ---");
const entryPoint = resolveAcpEntryPoint();
console.log(`Entry: ${entryPoint}`);

// 4. Determine output name
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

// 5. Rename to include target triple (Tauri convention)
if (existsSync(finalBin)) rmSync(finalBin);
renameSync(tmpBin, finalBin);

console.log(`\n=== Done: ${finalBin} ===`);
console.log("Next: bun run --cwd apps/cockpit tauri build  (or tauri dev for smoke test)");
