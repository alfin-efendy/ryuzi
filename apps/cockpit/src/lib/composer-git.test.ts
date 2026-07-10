import { expect, test } from "bun:test";
import { composerGitOptions, composerGitOptionsForProject, newBranchNameError, normalizeBranchName } from "./composer-git";
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

test("modal-created name (not in list) derives create intent — the New Branch modal path", () => {
  expect(composerGitOptions(list, "feat/from-modal", true)).toEqual({
    useWorktree: true,
    createBranch: true,
    branchName: "feat/from-modal",
    baseBranch: null,
  });
});

test("newBranchNameError: empty, whitespace, existing, valid", () => {
  expect(newBranchNameError("", ["main"])).toBe("Branch name is required");
  expect(newBranchNameError("has space", ["main"])).toBe("Branch names can't contain spaces");
  expect(newBranchNameError("has\ttab", ["main"])).toBe("Branch names can't contain spaces");
  expect(newBranchNameError("main", ["main", "develop"])).toBe('Branch "main" already exists');
  expect(newBranchNameError("feat/x", ["main", "develop"])).toBeNull();
});

test("non-git project → null (GitOptions are never sent)", () => {
  expect(composerGitOptionsForProject(false, list, "develop", true)).toBeNull();
  expect(composerGitOptionsForProject(false, null, null, true)).toBeNull();
});

test("git project → delegates to composerGitOptions", () => {
  expect(composerGitOptionsForProject(true, list, "develop", false)).toEqual({
    useWorktree: false,
    createBranch: false,
    branchName: null,
    baseBranch: "develop",
  });
});

test("normalizeBranchName: whitespace runs become single dashes", () => {
  expect(normalizeBranchName("my new feature")).toBe("my-new-feature");
  expect(normalizeBranchName("a  b\tc")).toBe("a-b-c");
  expect(normalizeBranchName("already-fine")).toBe("already-fine");
  expect(normalizeBranchName("")).toBe("");
});

test("normalizeBranchName: whitespace-and-dash runs collapse to one dash; pure dashes stay", () => {
  expect(normalizeBranchName("my- new")).toBe("my-new"); // live typing: "my-" + " new" collapses
  expect(normalizeBranchName("my-- x")).toBe("my-x");
  expect(normalizeBranchName("feat-x")).toBe("feat-x"); // intentional dashes untouched
});
