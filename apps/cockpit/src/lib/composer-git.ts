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

/** Project-aware wrapper: non-git projects must never send GitOptions —
 *  the backend then skips workspace prep and runs in the project workdir. */
export function composerGitOptionsForProject(
  isGit: boolean,
  list: BranchList | null,
  selected: string | null,
  useWorktree: boolean,
): GitOptions | null {
  if (!isGit) return null;
  return composerGitOptions(list, selected, useWorktree);
}

/** Client-side validation for the New Branch modal: non-empty, no
 *  whitespace, not an existing branch. Returns a user-facing error, or null
 *  when the name is acceptable. (Full git ref-name rules are enforced by the
 *  backend at session start.) */
export function newBranchNameError(name: string, existing: string[]): string | null {
  if (name.length === 0) return "Branch name is required";
  if (/\s/.test(name)) return "Branch names can't contain spaces";
  if (existing.includes(name)) return `Branch "${name}" already exists`;
  return null;
}
