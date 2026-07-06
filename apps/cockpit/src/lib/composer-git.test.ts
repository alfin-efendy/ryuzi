import { expect, test } from "bun:test";
import type { BranchList } from "../bindings";
import { composerGitOptions } from "./composer-git";

const list: BranchList = { branches: ["main", "develop"], current: "main", detached: false };

test("existing branch + new branch ON: auto-named branch cut from the selection", () => {
  expect(composerGitOptions(list, "develop", true, true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: null,
    baseBranch: "develop",
  });
});

test("free-typed name + new branch ON: createBranch with the typed name, base = HEAD", () => {
  expect(composerGitOptions(list, "feat/login", false, true)).toEqual({
    useWorktree: false,
    createBranch: true,
    branchName: "feat/login",
    baseBranch: null,
  });
});

test("existing branch + new branch OFF: run on the selected branch", () => {
  expect(composerGitOptions(list, "develop", true, false)).toEqual({
    useWorktree: true,
    createBranch: false,
    branchName: null,
    baseBranch: "develop",
  });
});

test("nothing selected (list not loaded): engine defaults", () => {
  expect(composerGitOptions(null, null, true, true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: null,
    baseBranch: null,
  });
});

test("stale selection not in the list with new branch OFF falls back to no base", () => {
  expect(composerGitOptions(list, "gone", true, false)).toEqual({
    useWorktree: true,
    createBranch: false,
    branchName: null,
    baseBranch: null,
  });
});
