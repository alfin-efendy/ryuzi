#!/usr/bin/env node
"use strict";

const { spawn } = require("node:child_process");
const fs = require("node:fs");

const BINARY_NAME = process.platform === "win32" ? "hr.exe" : "hr";

const PLATFORM_PACKAGES = {
  "linux x64 glibc": "harness-router-linux-x64",
  "linux x64 musl": "harness-router-linux-x64-musl",
  "linux arm64 glibc": "harness-router-linux-arm64",
  "linux arm64 musl": "harness-router-linux-arm64-musl",
  "darwin x64": "harness-router-darwin-x64",
  "darwin arm64": "harness-router-darwin-arm64",
  "win32 x64": "harness-router-win32-x64",
};

function isMusl() {
  let report = null;
  try {
    report = typeof process.report?.getReport === "function" ? process.report.getReport() : null;
  } catch {
    report = null;
  }
  if (report && report.header && report.header.glibcVersionRuntime) return false;
  if (report && Array.isArray(report.sharedObjects)) {
    if (report.sharedObjects.some((o) => /\/(?:libc\.musl-|ld-musl-)/.test(o))) return true;
  }
  try {
    const { execSync } = require("node:child_process");
    const out = execSync("ldd --version 2>&1 || true", { encoding: "utf8" });
    if (out.includes("musl")) return true;
    if (out.includes("GNU C Library") || out.includes("glibc")) return false;
  } catch {
    /* ignore */
  }
  return false; // assume glibc
}

function platformKey() {
  const { platform, arch } = process;
  if (platform === "linux") return `linux ${arch} ${isMusl() ? "musl" : "glibc"}`;
  return `${platform} ${arch}`;
}

function fail(msg) {
  console.error(`\n[harness-router] ${msg}\n`);
  process.exit(1);
}

function resolveBinaryPath() {
  const override = process.env.HR_BINARY_PATH;
  if (override) {
    if (fs.existsSync(override)) return override;
    console.warn(`[harness-router] HR_BINARY_PATH is set to "${override}" but that path does not exist — ignoring it.`);
  }

  const key = platformKey();
  const pkg = PLATFORM_PACKAGES[key];
  if (!pkg) {
    fail(
      `no prebuilt binary for your platform (${key}).\n` +
        `Supported: ${Object.keys(PLATFORM_PACKAGES).join(", ")}.`,
    );
  }
  try {
    return require.resolve(`${pkg}/${BINARY_NAME}`);
  } catch {
    fail(
      `could not find the binary package "${pkg}" for your platform (${key}).\n\n` +
        `It should have installed automatically as an optional dependency. Likely causes:\n` +
        `  - install ran with --no-optional / --omit=optional\n` +
        `  - node_modules was built on a different OS/arch and copied here\n` +
        `  - the optional dependency failed to download\n\n` +
        `Fix: reinstall on this machine: npm install -g harness-router`,
    );
  }
}

function run() {
  const binPath = resolveBinaryPath();
  const child = spawn(binPath, process.argv.slice(2), { stdio: "inherit", windowsHide: false });

  const signals = ["SIGINT", "SIGTERM", "SIGHUP", "SIGQUIT"];
  const forward = (sig) => {
    if (!child.killed) {
      try {
        child.kill(sig);
      } catch {
        /* ignore */
      }
    }
  };
  for (const sig of signals) process.on(sig, () => forward(sig));

  child.on("error", (err) => fail(`failed to launch ${binPath}:\n${err.message}`));
  child.on("exit", (code, signal) => {
    for (const sig of signals) process.removeAllListeners(sig);
    if (signal) process.kill(process.pid, signal);
    else process.exit(code == null ? 1 : code);
  });
}

run();
