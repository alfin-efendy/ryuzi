import { afterEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useStore, type PendingApproval } from "@/store";

const { ApprovalCard } = await import("./ApprovalCard");

afterEach(cleanup);

function approval(partial: Partial<PendingApproval>): PendingApproval {
  return {
    sessionPk: "s1",
    requestId: "r1",
    tool: "bash",
    summary: "Bash: rm -rf ./x",
    kind: "tool",
    input: { command: "rm -rf ./x" },
    principal: null,
    ...partial,
  };
}

function seedResolve() {
  const calls: unknown[] = [];
  useStore.setState({
    resolveApproval: async (id, resp) => {
      calls.push([id, resp]);
    },
  });
  return calls;
}

test("bash card shows the full command and resolves allowOnce", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  expect(screen.getByText("rm -rf ./x")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  expect(calls[0]).toEqual(["r1", { decision: "allowOnce", scope: null, payload: null }]);
});

test("bash card resolves rejectOnce on Deny", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  expect(calls[0]).toEqual(["r1", { decision: "rejectOnce", scope: null, payload: null }]);
});

test("allow scope menu sends allowAlways+session and allowAlways+project", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.click(screen.getByRole("button", { name: "Allow options" }));
  fireEvent.click(screen.getByRole("button", { name: "Allow for this session" }));
  expect(calls[0]).toEqual(["r1", { decision: "allowAlways", scope: "session", payload: null }]);

  fireEvent.click(screen.getByRole("button", { name: "Allow options" }));
  fireEvent.click(screen.getByRole("button", { name: "Always allow in this project" }));
  expect(calls[1]).toEqual(["r1", { decision: "allowAlways", scope: "project", payload: null }]);
});

test("deny scope menu sends rejectAlways+project", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.click(screen.getByRole("button", { name: "Deny options" }));
  fireEvent.click(screen.getByRole("button", { name: "Always deny in this project" }));
  expect(calls[0]).toEqual(["r1", { decision: "rejectAlways", scope: "project", payload: null }]);
});

test("plan card renders the plan and reject reveals a feedback field", () => {
  render(<ApprovalCard approval={approval({ kind: "plan", tool: "exitplanmode", input: { plan: "# My plan\ndo X" } })} />);
  expect(screen.getByText("My plan")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reject with feedback" }));
  expect(screen.getByLabelText("Feedback")).toBeTruthy();
});

test("plan rejection sends the typed feedback", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({ kind: "plan", tool: "exitplanmode", input: { plan: "# My plan\ndo X" } })} />);
  fireEvent.click(screen.getByRole("button", { name: "Reject with feedback" }));
  fireEvent.change(screen.getByLabelText("Feedback"), { target: { value: "needs tests" } });
  fireEvent.click(screen.getByRole("button", { name: "Send rejection" }));
  expect(calls[0]).toEqual(["r1", { decision: "rejectOnce", scope: null, payload: { feedback: "needs tests" } }]);
});

test("plan approval sends the chosen edit mode", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({ kind: "plan", tool: "exitplanmode", input: { plan: "# My plan\ndo X" } })} />);
  fireEvent.click(screen.getByRole("button", { name: "Approve — auto-approve edits" }));
  expect(calls[0]).toEqual(["r1", { decision: "allowOnce", scope: null, payload: { mode: "acceptEdits" } }]);
});

test("question card submits selected labels", async () => {
  const calls = seedResolve();
  render(
    <ApprovalCard
      approval={approval({
        kind: "question",
        tool: "askuserquestion",
        input: {
          questions: [
            {
              question: "Which DB?",
              header: "Database",
              multiSelect: false,
              options: [{ label: "SQLite" }, { label: "Postgres" }],
            },
          ],
        },
      })}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /SQLite/ }));
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  const [, resp] = calls[0] as [string, { payload: { answers: Record<string, string[]> } }];
  expect(resp.payload.answers["Which DB?"]).toEqual(["SQLite"]);
});

test("question card appends the per-question Other answer", async () => {
  const calls = seedResolve();
  render(
    <ApprovalCard
      approval={approval({
        kind: "question",
        tool: "askuserquestion",
        input: {
          questions: [
            {
              question: "Which DB?",
              header: "Database",
              multiSelect: true,
              options: [{ label: "SQLite" }, { label: "Postgres" }],
            },
          ],
        },
      })}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /SQLite/ }));
  fireEvent.change(screen.getByLabelText("Other answer for Database"), { target: { value: "MongoDB" } });
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  const [, resp] = calls[0] as [string, { payload: { answers: Record<string, string[]> } }];
  expect(resp.payload.answers["Which DB?"]).toEqual(["SQLite", "MongoDB"]);
});

test("edit card shows before/after blocks", () => {
  render(
    <ApprovalCard
      approval={approval({
        tool: "edit",
        summary: "Edit: src/a.ts",
        input: { file_path: "src/a.ts", old_string: "const a = 1", new_string: "const a = 2" },
      })}
    />,
  );
  expect(screen.getByText("const a = 1")).toBeTruthy();
  expect(screen.getByText("const a = 2")).toBeTruthy();
});

test("generic tool renders summary and a collapsible parameter dump", () => {
  render(
    <ApprovalCard
      approval={approval({
        tool: "webfetch",
        summary: "Fetch: https://example.com",
        input: { url: "https://example.com" },
      })}
    />,
  );
  expect(screen.getByText("Fetch: https://example.com")).toBeTruthy();
  expect(screen.getByText("Parameters")).toBeTruthy();
});

test("hotkey fires the primary action on Cmd/Ctrl+Enter", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} hotkey />);
  fireEvent.keyDown(window, { key: "Enter", metaKey: true });
  expect(calls[0]).toEqual(["r1", { decision: "allowOnce", scope: null, payload: null }]);
});

test("without hotkey, Cmd/Ctrl+Enter does nothing", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.keyDown(window, { key: "Enter", metaKey: true });
  expect(calls.length).toBe(0);
});

test("shows a 'via <plugin>' chip when the approval carries a principal", () => {
  render(
    <ApprovalCard
      approval={approval({
        tool: "mcp__github__search_issues",
        summary: "run mcp__github__search_issues",
        principal: { pluginId: "github-connector", pluginName: "GitHub" },
      })}
    />,
  );
  expect(screen.getByText("via GitHub")).toBeTruthy();
});

test("hides the principal chip when the approval has no principal", () => {
  render(<ApprovalCard approval={approval({})} />);
  expect(screen.queryByText(/^via /)).toBeNull();
});

test("plan card hotkey submits the rejection instead of approving while feedback is open", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({ kind: "plan", tool: "exitplanmode", input: { plan: "# My plan\ndo X" } })} hotkey />);
  fireEvent.click(screen.getByRole("button", { name: "Reject with feedback" }));
  fireEvent.change(screen.getByLabelText("Feedback"), { target: { value: "needs more tests" } });
  fireEvent.keyDown(window, { key: "Enter", metaKey: true });
  expect(calls[0]).toEqual(["r1", { decision: "rejectOnce", scope: null, payload: { feedback: "needs more tests" } }]);
});
