import { describe, expect, test, afterEach } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentSummaryInfo } from "@/bindings";
import type { ContextPickerGroup } from "@/lib/composer-context";
import { ContextPickerMenu } from "./ContextPickerMenu";

afterEach(cleanup);

const ada: AgentSummaryInfo = {
  id: "ada",
  name: "Ada",
  description: "Accessibility review",
  avatarColor: "blue",
  model: { kind: "route", route: "free" },
  permissionMode: "ask",
  skillCount: 0,
  toolCount: 0,
  knowledgeCount: 0,
  executable: true,
  validation: [],
  isDefault: false,
};

const groups: ContextPickerGroup[] = [
  { section: "project", label: "Project", items: [{ kind: "project", id: "proj1", name: "Acme Web" }] },
  { section: "agents", label: "Agents", items: [{ kind: "agent", agent: ada }] },
  {
    section: "folders",
    label: "Folders",
    items: [
      { kind: "workspace", path: "src/lib", dir: true },
      { kind: "workspace", path: "src/views", dir: true },
    ],
  },
  {
    section: "files",
    label: "Files",
    items: [{ kind: "workspace", path: "README.md", dir: false }],
  },
];

describe("ContextPickerMenu", () => {
  test("renders all four sections with their labels", () => {
    render(<ContextPickerMenu groups={groups} activeIndex={0} onPick={() => undefined} onClose={() => undefined} />);

    expect(screen.getByText("Project")).toBeTruthy();
    expect(screen.getByText("Agents")).toBeTruthy();
    expect(screen.getByText("Folders")).toBeTruthy();
    expect(screen.getByText("Files")).toBeTruthy();
  });

  test("shows a Current project detail on the project row", () => {
    render(<ContextPickerMenu groups={groups} activeIndex={0} onPick={() => undefined} onClose={() => undefined} />);

    expect(screen.getByText("Current project")).toBeTruthy();
    expect(screen.getByText("Acme Web")).toBeTruthy();
  });

  test("marks the row at the flattened cross-section index as active", () => {
    // Flattened order: [project, agent, folder src/lib, folder src/views, file README.md]
    // index 3 -> "src/views" folder row, in the third section.
    render(<ContextPickerMenu groups={groups} activeIndex={3} onPick={() => undefined} onClose={() => undefined} />);

    const activeRow = screen.getByText("src/views").closest("button");
    const otherRow = screen.getByText("src/lib").closest("button");
    // Every row already has a leading kind icon (Folder/FileText/dot), so the
    // check icon MenuPanelItem appends when `selected` shows up as a *second*
    // svg only on the active row.
    expect(activeRow?.querySelectorAll("svg").length).toBe(2);
    expect(otherRow?.querySelectorAll("svg").length).toBe(1);
  });

  test("clicking a folder row calls onPick with the exact folder item", () => {
    const picked: unknown[] = [];
    render(<ContextPickerMenu groups={groups} activeIndex={0} onPick={(item) => picked.push(item)} onClose={() => undefined} />);

    fireEvent.click(screen.getByText("src/lib"));

    expect(picked).toEqual([{ kind: "workspace", path: "src/lib", dir: true }]);
  });

  test("renders as a menu with the expected positioning classes", () => {
    const { container } = render(<ContextPickerMenu groups={groups} activeIndex={0} onPick={() => undefined} onClose={() => undefined} />);

    expect(screen.getByRole("menu")).toBeTruthy();
    const panel = container.querySelector(".bottom-full.left-2\\.5.z-50.mb-1\\.5.w-\\[360px\\]");
    expect(panel).toBeTruthy();
  });
});
