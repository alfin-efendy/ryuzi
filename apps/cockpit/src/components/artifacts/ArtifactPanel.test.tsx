import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const listSessionArtifacts = mock(() => Promise.resolve({ status: "ok" as const, data: [{
  id: "artifact-1", sourceSessionPk: "s1", referenceId: null, sharedFromSessionPk: null,
  parentReferenceId: null, status: "available", name: "report.md", contentType: "text/markdown",
  sizeBytes: 5, creator: "agent", createdAt: 1, sha256: "abc",
}] }));
const fetchArtifact = mock(() => Promise.resolve({ status: "ok" as const, data: { name: "report.md", contentType: "text/markdown", dataBase64: "aGVsbG8=" } }));

mock.module("@/bindings", () => ({ commands: { listSessionArtifacts, fetchArtifact } }));
const { ArtifactPanel } = await import("./ArtifactPanel");

afterEach(() => {
  cleanup();
  listSessionArtifacts.mockClear();
  fetchArtifact.mockClear();
});

test("lists session artifacts and opens a safe preview", async () => {
  render(<ArtifactPanel runnerId="local" sessionPk="s1" />);
  expect(await screen.findByText("report.md")).toBeTruthy();
  expect(screen.getByText("Available")).toBeTruthy();
  fireEvent.click(screen.getByTitle("Preview"));
  expect(await screen.findByText("hello")).toBeTruthy();
  expect(fetchArtifact).toHaveBeenCalledWith("local", "s1", "artifact-1");
});

test("marks shared deleted artifacts unavailable", async () => {
  listSessionArtifacts.mockResolvedValueOnce({ status: "ok", data: [{
    id: "artifact-2", sourceSessionPk: "s1", referenceId: "ref-1", sharedFromSessionPk: "s1",
    parentReferenceId: null, status: "deleted", name: "old.zip", contentType: "application/zip",
    sizeBytes: 7, creator: "user", createdAt: 1, sha256: "abc",
  }] });
  render(<ArtifactPanel runnerId="local" sessionPk="s2" />);
  expect(await screen.findByText("Deleted after retention")).toBeTruthy();
  expect(screen.getByTitle("Preview")).toHaveProperty("disabled", true);
});
