import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

export function worktreePathFor(workdirRoot: string, projectId: string, sessionPk: string): string {
  return join(workdirRoot, ".harness-worktrees", projectId, sessionPk);
}

export async function createWorktree(repoDir: string, worktreePath: string, branch: string): Promise<void> {
  // git worktree add does not create leading directories — ensure the parent exists.
  mkdirSync(dirname(worktreePath), { recursive: true });
  await Bun.$`git -C ${repoDir} worktree add -b ${branch} ${worktreePath}`.quiet();
}

export async function removeWorktree(repoDir: string, worktreePath: string): Promise<void> {
  await Bun.$`git -C ${repoDir} worktree remove --force ${worktreePath}`.quiet();
}
