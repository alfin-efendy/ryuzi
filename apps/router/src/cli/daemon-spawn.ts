/** True when running inside a `bun build --compile` standalone executable. */
export function isCompiledExecutable(main: string = Bun.main): boolean {
  return main.startsWith("/$bunfs/") || main.startsWith("B:\\~BUN") || main.includes("/~BUN/");
}

/**
 * Command array to relaunch this program in hidden `__daemon` mode.
 * Compiled: the embedded entry auto-runs, so pass only the subcommand.
 * Dev (`bun <script>`): pass the script path so Bun knows what to execute.
 */
export function daemonRelaunchCmd(opts: { execPath: string; main: string; compiled: boolean }): string[] {
  return opts.compiled
    ? [opts.execPath, "__daemon"]
    : [opts.execPath, opts.main, "__daemon"];
}
