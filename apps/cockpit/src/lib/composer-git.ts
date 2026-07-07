import type { BranchList, GitOptions } from "../bindings";

/** Map the composer's branch selection + toggle chips onto start_session git
 *  options (the backend behavior matrix). A selected value that is not in the
 *  loaded branch list is a free-typed new branch name — only reachable when
 *  "New branch" is ON, since the Combobox only allows create in that mode. */
export function composerGitOptions(
  list: BranchList | null,
  selected: string | null,
  useWorktree: boolean,
  createBranch: boolean,
): GitOptions {
  const known = list?.branches ?? [];
  const isExisting = selected !== null && known.includes(selected);
  if (createBranch && selected !== null && !isExisting) {
    return { useWorktree, createBranch: true, branchName: selected, baseBranch: null };
  }
  return {
    useWorktree,
    createBranch,
    branchName: null,
    baseBranch: isExisting ? selected : null,
  };
}
