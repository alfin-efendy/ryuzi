#!/usr/bin/env bun
import { runCli, type IO } from "./run";
import { detectClaude, detectGit } from "@harness/core";

process.on("unhandledRejection", (reason) => {
  console.error("unhandledRejection:", reason);
});

function defaultDbPath(): string {
  const dir = `${process.env.HOME ?? "."}/.local/share/harness-router`;
  return `${dir}/harness.sqlite`;
}

async function promptStdin(q: string): Promise<string> {
  process.stdout.write(q);
  for await (const line of console) return line;
  return "";
}

const io: IO = {
  out: (s) => console.log(s),
  err: (s) => console.error(s),
  prompt: promptStdin,
};

const dbPath = defaultDbPath();
await Bun.$`mkdir -p ${dbPath.slice(0, dbPath.lastIndexOf("/"))}`.quiet();

const code = await runCli(process.argv.slice(2), {
  io,
  dbPath,
  detect: { claude: detectClaude, git: detectGit },
});
process.exit(code);
