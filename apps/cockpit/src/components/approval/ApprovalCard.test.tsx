import { afterEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useStore, type PendingApproval } from "@/store";
import { LOCAL_RUNNER } from "@/lib/session-key";

const { ApprovalCard } = await import("./ApprovalCard");

afterEach(cleanup);

function approval(partial: Partial<PendingApproval>): PendingApproval {
  return {
    runnerId: LOCAL_RUNNER,
    sessionPk: "s1",
    runId: "run-1",
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
    resolveApproval: async (_runnerId, runId, id, resp) => {
      calls.push([runId, id, resp]);
    },
  });
  return calls;
}

test("bash card shows the full command and resolves allowOnce", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  expect(screen.getByText("rm -rf ./x")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "allowOnce", scope: null, payload: null }]);
});

test("bash card resolves rejectOnce on Deny", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "rejectOnce", scope: null, payload: null }]);
});

test("allow scope menu sends allowAlways+session and allowAlways+project", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.click(screen.getByRole("button", { name: "Allow options" }));
  fireEvent.click(screen.getByRole("button", { name: "Allow for this session" }));
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "allowAlways", scope: "session", payload: null }]);

  fireEvent.click(screen.getByRole("button", { name: "Allow options" }));
  fireEvent.click(screen.getByRole("button", { name: "Always allow in this project" }));
  expect(calls[1]).toEqual(["run-1", "r1", { decision: "allowAlways", scope: "project", payload: null }]);
});

test("deny scope menu sends rejectAlways+project", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({})} />);
  fireEvent.click(screen.getByRole("button", { name: "Deny options" }));
  fireEvent.click(screen.getByRole("button", { name: "Always deny in this project" }));
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "rejectAlways", scope: "project", payload: null }]);
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
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "rejectOnce", scope: null, payload: { feedback: "needs tests" } }]);
});

test("plan approval sends the chosen edit mode", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={approval({ kind: "plan", tool: "exitplanmode", input: { plan: "# My plan\ndo X" } })} />);
  fireEvent.click(screen.getByRole("button", { name: "Approve — auto-approve edits" }));
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "allowOnce", scope: null, payload: { mode: "acceptEdits" } }]);
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
  const [, , resp] = calls[0] as [string, string, { payload: { answers: Record<string, string[]> } }];
  expect(resp.payload.answers["Which DB?"]).toEqual(["SQLite"]);
});

test("question card option buttons expose aria-pressed reflecting selection", async () => {
  seedResolve();
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
  const sqliteBtn = screen.getByRole("button", { name: /SQLite/ });
  expect(sqliteBtn.getAttribute("aria-pressed")).toBe("false");
  fireEvent.click(sqliteBtn);
  expect(sqliteBtn.getAttribute("aria-pressed")).toBe("true");
});

function multiQuestionApproval(partial: Partial<PendingApproval> = {}) {
  return approval({
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
        {
          question: "Which cache?",
          header: "Cache",
          multiSelect: false,
          options: [{ label: "Redis" }, { label: "Memcached" }],
        },
      ],
    },
    ...partial,
  });
}

test("question card exposes the active prompt as a heading", () => {
  render(<ApprovalCard approval={multiQuestionApproval()} />);
  expect(screen.getByRole("heading", { name: "Which DB?" })).toBeTruthy();
});

test("question card shows exactly one question at a time with N of M progress", () => {
  render(<ApprovalCard approval={multiQuestionApproval()} />);
  expect(screen.getByText("Question 1 of 2")).toBeTruthy();
  expect(screen.getByText("Which DB?")).toBeTruthy();
  expect(screen.queryByText("Which cache?")).toBeNull();

  const progress = screen.getByRole("progressbar", { name: "Question progress" });
  expect(progress.getAttribute("aria-valuenow")).toBe("1");
  expect(progress.getAttribute("aria-valuemax")).toBe("2");

  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("Question 2 of 2")).toBeTruthy();
  expect(screen.queryByText("Which DB?")).toBeNull();
  expect(screen.getByText("Which cache?")).toBeTruthy();
  expect(screen.getByRole("progressbar", { name: "Question progress" }).getAttribute("aria-valuenow")).toBe("2");
});

test("question card shows Back only after the first question and Next before the final one", () => {
  render(<ApprovalCard approval={multiQuestionApproval()} />);
  expect(screen.queryByRole("button", { name: "Back" })).toBeNull();
  expect(screen.getByRole("button", { name: "Next" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Submit" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByRole("button", { name: "Back" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Next" })).toBeNull();
  expect(screen.getByRole("button", { name: "Submit" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  expect(screen.queryByRole("button", { name: "Back" })).toBeNull();
  expect(screen.getByRole("button", { name: "Next" })).toBeTruthy();
});

test("question card Dismiss is available on every step", () => {
  render(<ApprovalCard approval={multiQuestionApproval()} />);
  expect(screen.getByRole("button", { name: "Dismiss" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByRole("button", { name: "Dismiss" })).toBeTruthy();
});

test("question card preserves selections and Other text across Back/Next and submits merged answers", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={multiQuestionApproval()} />);
  fireEvent.click(screen.getByRole("button", { name: /SQLite/ }));
  fireEvent.change(screen.getByLabelText("Other answer for Database"), { target: { value: "MongoDB" } });
  fireEvent.click(screen.getByRole("button", { name: "Next" }));

  fireEvent.click(screen.getByRole("button", { name: /Redis/ }));
  fireEvent.click(screen.getByRole("button", { name: "Back" }));

  expect(screen.getByRole("button", { name: /SQLite/ }).getAttribute("aria-pressed")).toBe("true");
  expect((screen.getByLabelText("Other answer for Database") as HTMLInputElement).value).toBe("MongoDB");

  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByRole("button", { name: /Redis/ }).getAttribute("aria-pressed")).toBe("true");

  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  const [, , resp] = calls[0] as [string, string, { payload: { answers: Record<string, string[]> } }];
  expect(resp.payload.answers["Which DB?"]).toEqual(["SQLite", "MongoDB"]);
  expect(resp.payload.answers["Which cache?"]).toEqual(["Redis"]);
});

test("question card allows submitting with no answer selected (answers optional)", async () => {
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
  fireEvent.click(screen.getByRole("button", { name: "Submit" }));
  const [, , resp] = calls[0] as [string, string, { payload: { answers: Record<string, string[]> } }];
  expect(resp.payload.answers["Which DB?"]).toEqual([]);
});

test("question card with no questions shows a fallback message with Dismiss and no Submit", async () => {
  const calls = seedResolve();
  render(
    <ApprovalCard
      approval={approval({
        kind: "question",
        tool: "askuserquestion",
        input: { questions: [] },
      })}
    />,
  );
  expect(screen.getByText("No questions were provided.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Submit" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "rejectOnce", scope: null, payload: null }]);
});

test("question card hotkey advances through questions and submits on the last with Ctrl/Cmd+Enter", async () => {
  const calls = seedResolve();
  render(<ApprovalCard approval={multiQuestionApproval()} hotkey />);
  fireEvent.keyDown(window, { key: "Enter", metaKey: true });
  expect(calls.length).toBe(0);
  expect(screen.getByText("Question 2 of 2")).toBeTruthy();

  fireEvent.keyDown(window, { key: "Enter", metaKey: true });
  expect(calls.length).toBe(1);
  const [, , resp] = calls[0] as [string, string, { payload: { answers: Record<string, string[]> } }];
  expect(resp.payload.answers).toEqual({ "Which DB?": [], "Which cache?": [] });
});

test("question card safely resets a later step when a new request has fewer questions", () => {
  const { rerender } = render(<ApprovalCard approval={multiQuestionApproval({ requestId: "r1" })} />);
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("Question 2 of 2")).toBeTruthy();

  rerender(
    <ApprovalCard
      approval={approval({
        requestId: "r2",
        kind: "question",
        tool: "askuserquestion",
        input: {
          questions: [
            {
              question: "Which queue?",
              header: "Queue",
              multiSelect: false,
              options: [{ label: "SQS" }, { label: "RabbitMQ" }],
            },
          ],
        },
      })}
    />,
  );

  expect(screen.getByText("Question 1 of 1")).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Which queue?" })).toBeTruthy();
  expect(screen.queryByText("Which cache?")).toBeNull();
  expect(screen.getByRole("button", { name: /SQS/ }).getAttribute("aria-pressed")).toBe("false");
  expect((screen.getByLabelText("Other answer for Queue") as HTMLInputElement).value).toBe("");
});

test("question card resets step, answers and Other text when approval.requestId changes", () => {
  const first = multiQuestionApproval({ requestId: "r1" });
  const { rerender } = render(<ApprovalCard approval={first} />);
  fireEvent.click(screen.getByRole("button", { name: /SQLite/ }));
  fireEvent.change(screen.getByLabelText("Other answer for Database"), { target: { value: "MongoDB" } });
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("Question 2 of 2")).toBeTruthy();

  const second = multiQuestionApproval({ requestId: "r2" });
  rerender(<ApprovalCard approval={second} />);

  expect(screen.getByText("Question 1 of 2")).toBeTruthy();
  expect(screen.getByRole("button", { name: /SQLite/ }).getAttribute("aria-pressed")).toBe("false");
  expect((screen.getByLabelText("Other answer for Database") as HTMLInputElement).value).toBe("");
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
  const [, , resp] = calls[0] as [string, string, { payload: { answers: Record<string, string[]> } }];
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
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "allowOnce", scope: null, payload: null }]);
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
  expect(calls[0]).toEqual(["run-1", "r1", { decision: "rejectOnce", scope: null, payload: { feedback: "needs more tests" } }]);
});
