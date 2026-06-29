// Expand a leading `~` / `~/` to $HOME. `~` is a shell-ism; Node/Bun path ops and
// git do NOT expand it, so a stored `workdir_root` like `~/repos` must be expanded
// before use or it becomes a literal `~` directory and breaks worktree/cwd resolution.
export function expandHome(p: string): string {
  const home = process.env.HOME;
  if (!home) return p;
  if (p === "~") return home;
  if (p.startsWith("~/")) return home + p.slice(1);
  return p;
}
