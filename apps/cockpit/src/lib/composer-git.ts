import type { BranchList, GitOptions } from "../bindings";

/** Map the composer's branch selection onto start_session git options.
 *  Create-vs-existing intent is DERIVED from the selection (no toggle):
 *  - null / the current branch → engine-named new branch from HEAD (the
 *    current branch is already checked out, so a worktree can't reuse it);
 *  - another existing branch → work on that branch;
 *  - a typed name not in the list → create that named branch. */
export function composerGitOptions(list: BranchList | null, selected: string | null, useWorktree: boolean): GitOptions {
  const known = list?.branches ?? [];
  const current = list?.current ?? null;
  if (selected !== null && !known.includes(selected)) {
    return { useWorktree, createBranch: true, branchName: selected, baseBranch: null };
  }
  if (selected !== null && selected !== current) {
    return { useWorktree, createBranch: false, branchName: null, baseBranch: selected };
  }
  return { useWorktree, createBranch: true, branchName: null, baseBranch: null };
}
