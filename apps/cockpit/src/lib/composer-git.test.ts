import { expect, test } from "bun:test";
import { composerGitOptions } from "./composer-git";
import type { BranchList } from "../bindings";

const list: BranchList = { branches: ["main", "develop"], current: "main", detached: false };

test("null selection → engine-named new branch from HEAD", () => {
  expect(composerGitOptions(list, null, true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: null,
    baseBranch: null,
  });
});

test("current branch selected → same as null (new engine-named branch)", () => {
  expect(composerGitOptions(list, "main", true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: null,
    baseBranch: null,
  });
});

test("other existing branch → work on it (no new branch)", () => {
  expect(composerGitOptions(list, "develop", false)).toEqual({
    useWorktree: false,
    createBranch: false,
    branchName: null,
    baseBranch: "develop",
  });
});

test("typed new name → create that named branch", () => {
  expect(composerGitOptions(list, "feat/x", true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: "feat/x",
    baseBranch: null,
  });
});

test("no branch list loaded → typed value still creates", () => {
  expect(composerGitOptions(null, "feat/y", true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: "feat/y",
    baseBranch: null,
  });
});
