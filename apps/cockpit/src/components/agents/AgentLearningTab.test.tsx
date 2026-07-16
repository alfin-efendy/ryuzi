import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AgentLearningInfo, KnowledgeConceptInfo } from "@/bindings";

const validRaw = `---\ntitle: Fixed memory\ndescription: Repaired\ntype: memory\nscope: user\ntags: []\ntimestamp: 2026-03-01T00:00:00Z\n---\nUse concise summaries.`;
const parsed: KnowledgeConceptInfo = {
  id: "fixed-memory",
  relativePath: "memory/user/broken.md",
  conceptType: "memory",
  title: "Fixed memory",
  description: "Repaired",
  body: "Use concise summaries.",
  scope: "user",
  projectId: null,
  tags: [],
  timestamp: "2026-03-01T00:00:00Z",
};
const reviewerLearning: AgentLearningInfo = {
  concepts: [{ ...parsed, id: "concise", relativePath: "memory/user/concise.md", title: "Prefer concise summaries" }],
  invalid: [{ relativePath: "memory/user/broken.md", error: "missing title", rawMarkdown: "broken" }],
  journey: [{ conceptId: "concise", title: "Learned review style", timestamp: "2026-03-01T00:00:00Z" }],
  skillUsage: [{ skillId: "requesting-code-review", uses: 3, successes: 2, conceptId: "concise" }],
  reviews: [{ conceptId: "concise", title: "Review update", description: "Stored preference", timestamp: "2026-03-02T00:00:00Z" }],
  curator: { concept: parsed, lastEventId: "event-1" },
  curatorHistory: [
    { snapshotId: "snapshot-new", concept: { ...parsed, title: "Newest knowledge" } },
    { snapshotId: "snapshot-old", concept: { ...parsed, title: "Earlier knowledge" } },
  ],
};

const load = mock(async (_agentId: string) => {});
const createConcept = mock(async () => true);
const updateConcept = mock(async () => true);
const deleteConcept = mock(async () => true);
const validateRaw = mock(async (): Promise<KnowledgeConceptInfo | null> => parsed);
const replaceRaw = mock(async () => true);
const deleteInvalid = mock(async () => true);
const rollback = mock(async () => true);

const { useLearning } = await import("@/store-learning");
const { useStore } = await import("@/store");
const { AgentLearningTab } = await import("./AgentLearningTab");

function seedLearning(snapshot: AgentLearningInfo) {
  useLearning.setState({
    byAgent: { reviewer: snapshot },
    loading: {},
    rollingBack: {},
    requestGeneration: {},
    load,
    createConcept,
    updateConcept,
    deleteConcept,
    validateRaw,
    replaceRaw,
    deleteInvalid,
    rollback,
  });
}

beforeEach(() => {
  for (const fn of [load, createConcept, updateConcept, deleteConcept, validateRaw, replaceRaw, deleteInvalid, rollback])
    fn.mockClear();
  useStore.setState({ projects: [] });
  seedLearning(reviewerLearning);
});
afterEach(cleanup);

test("Learning renders memory, journey, usage, reviews, curator, and repair sections", () => {
  render(<AgentLearningTab agentId="reviewer" />);
  for (const heading of ["Memory", "Journey", "Skill usage", "Reviews", "Curator", "Repair knowledge"]) {
    expect(screen.getByText(heading)).toBeTruthy();
  }
  expect(screen.getByText("Prefer concise summaries")).toBeTruthy();
  expect(screen.getByText("memory/user/broken.md")).toBeTruthy();
});

test("curator history preserves backend newest-first order", () => {
  render(<AgentLearningTab agentId="reviewer" />);
  const rollbackButtons = screen.getAllByRole("button", { name: /^Rollback / });
  expect(rollbackButtons.map((button) => button.getAttribute("aria-label"))).toEqual([
    "Rollback Newest knowledge",
    "Rollback Earlier knowledge",
  ]);
});

test("raw repair validates before Replace", async () => {
  render(<AgentLearningTab agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Repair memory/user/broken.md" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Raw Markdown" }), { target: { value: validRaw } });
  fireEvent.click(screen.getByRole("button", { name: "Validate" }));
  expect(validateRaw).toHaveBeenCalledWith("reviewer", "memory/user/broken.md", validRaw);
  expect(replaceRaw).not.toHaveBeenCalled();
  await waitFor(() => expect(screen.getByRole("button", { name: "Replace file" }).hasAttribute("disabled")).toBe(false));
});

test("changing raw markdown after validation disables Replace again", async () => {
  render(<AgentLearningTab agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Repair memory/user/broken.md" }));
  const raw = screen.getByRole("textbox", { name: "Raw Markdown" });
  fireEvent.change(raw, { target: { value: validRaw } });
  fireEvent.click(screen.getByRole("button", { name: "Validate" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Replace file" }).hasAttribute("disabled")).toBe(false));
  fireEvent.change(raw, { target: { value: `${validRaw}\nchanged` } });
  expect(screen.getByRole("button", { name: "Replace file" }).hasAttribute("disabled")).toBe(true);
});

test("validation proof is scoped to agent, path, and raw and ignores a late previous-target result", async () => {
  let resolveValidation!: (value: KnowledgeConceptInfo | null) => void;
  validateRaw.mockImplementationOnce(
    () =>
      new Promise<KnowledgeConceptInfo | null>((resolve) => {
        resolveValidation = resolve;
      }),
  );
  const { rerender } = render(<AgentLearningTab agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Repair memory/user/broken.md" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Raw Markdown" }), { target: { value: validRaw } });
  fireEvent.click(screen.getByRole("button", { name: "Validate" }));

  useLearning.setState({
    byAgent: {
      reviewer: reviewerLearning,
      ryuzi: {
        ...reviewerLearning,
        invalid: [{ relativePath: "memory/user/other.md", error: "missing title", rawMarkdown: "broken" }],
      },
    },
  });
  rerender(<AgentLearningTab agentId="ryuzi" />);
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  fireEvent.click(screen.getByRole("button", { name: "Repair memory/user/other.md" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Raw Markdown" }), { target: { value: validRaw } });
  resolveValidation(parsed);

  await waitFor(() => expect(screen.getByRole("button", { name: "Replace file" }).hasAttribute("disabled")).toBe(true));
});

test("memory editor requires a trimmed description before saving", () => {
  render(<AgentLearningTab agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Add memory" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Title" }), { target: { value: "New memory" } });
  fireEvent.change(screen.getByRole("textbox", { name: "Description" }), { target: { value: "   " } });
  fireEvent.change(screen.getByRole("textbox", { name: "Body" }), { target: { value: "Body" } });
  const save = screen.getByRole("button", { name: "Save memory" });
  expect(save.hasAttribute("disabled")).toBe(true);
  fireEvent.click(save);
  expect(createConcept).not.toHaveBeenCalled();
});

test("rollback requires explicit confirmation with the non-agent-file warning", async () => {
  render(<AgentLearningTab agentId="reviewer" />);
  fireEvent.click(screen.getByRole("button", { name: "Rollback Earlier knowledge" }));
  expect(
    screen.getByText(
      "Restore knowledge snapshot Earlier knowledge? Agent YAML and transcripts are not changed. The restored OKF state is recorded as a new rollback event.",
    ),
  ).toBeTruthy();
  expect(rollback).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Restore snapshot" }));
  await waitFor(() => expect(rollback).toHaveBeenCalledWith("reviewer", "snapshot-old"));
});
