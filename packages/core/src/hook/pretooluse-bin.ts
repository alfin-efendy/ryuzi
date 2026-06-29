#!/usr/bin/env bun
import { runHook } from "./pretooluse";

const input = await Bun.stdin.text();
const { stdout, exitCode } = await runHook({ input, env: process.env, fetchFn: fetch });
process.stdout.write(stdout);
process.exit(exitCode);
