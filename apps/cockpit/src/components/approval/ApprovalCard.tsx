import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Input, MenuPanel, MenuPanelItem, Textarea } from "@ryuzi/ui";
import { Check, ChevronDown, ShieldAlert } from "lucide-react";
import { useStore, type PendingApproval } from "@/store";
import type { ApprovalResponse } from "@/bindings";
import { Markdown } from "@/components/transcript/Markdown";
import { Pill } from "@/components/common/bits";
import { isSession } from "@/lib/session-key";

type Question = {
  question: string;
  header: string;
  multiSelect?: boolean;
  options: { label: string; description?: string }[];
};

const once = (allow: boolean): ApprovalResponse => ({
  decision: allow ? "allowOnce" : "rejectOnce",
  scope: null,
  payload: null,
});

function ToolBody({ approval }: { approval: PendingApproval }) {
  const [showFullInput, setShowFullInput] = useState(false);
  const input = (approval.input ?? {}) as Record<string, unknown>;
  if (approval.tool === "bash" && typeof input.command === "string") {
    return (
      <pre className="max-h-64 overflow-y-auto rounded-md bg-muted/60 px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words">
        {input.command}
      </pre>
    );
  }
  if (approval.tool === "edit" && typeof input.old_string === "string") {
    return (
      <div className="space-y-2">
        <div className="font-mono text-[11px] text-muted-foreground">{String(input.file_path ?? "")}</div>
        <pre className="max-h-64 overflow-y-auto rounded-md border border-red-500/25 bg-red-500/10 px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words">
          {String(input.old_string)}
        </pre>
        <pre className="max-h-64 overflow-y-auto rounded-md border border-emerald-500/25 bg-emerald-500/10 px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words">
          {String(input.new_string ?? "")}
        </pre>
      </div>
    );
  }

  const formattedInput = JSON.stringify(input, null, 2);
  const inputPreview = formattedInput.length > 320 ? `${formattedInput.slice(0, 320)}…` : formattedInput;

  return (
    <div className="space-y-2">
      <div className="font-mono text-xs break-words whitespace-pre-wrap">{approval.summary}</div>
      {Object.keys(input).length > 0 && (
        <div className="space-y-1.5">
          <pre className="rounded-md bg-muted/60 px-3 py-2 font-mono text-[11px] whitespace-pre-wrap break-words">{inputPreview}</pre>
          <Button size="sm" variant="ghost" onClick={() => setShowFullInput((show) => !show)}>
            {showFullInput ? "Hide full input" : "Show full input"}
          </Button>
          {showFullInput && (
            <pre
              data-testid="approval-full-input"
              className="max-h-64 overflow-y-auto rounded-md bg-muted/60 px-3 py-2 font-mono text-[11px] whitespace-pre-wrap break-words"
            >
              {formattedInput}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Split "Allow"/"Deny" action: the left segment fires the once-only decision
 * immediately, the caret opens a MenuPanel with the don't-ask-again scopes
 * for that decision (session/project remembered via `tool_policies`).
 */
function ScopedAction({
  label,
  menuLabel,
  variant,
  onPrimary,
  items,
}: {
  label: string;
  menuLabel: string;
  variant?: "default" | "outline";
  onPrimary: () => void;
  items: { label: string; onClick: () => void }[];
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="relative flex">
      <Button size="sm" variant={variant} className="rounded-r-none" onClick={onPrimary}>
        {label}
      </Button>
      <Button
        size="sm"
        variant={variant}
        aria-label={menuLabel}
        aria-expanded={open}
        className="rounded-l-none border-l border-l-background/40 px-1.5"
        onClick={() => setOpen((v) => !v)}
      >
        <ChevronDown size={13} />
      </Button>
      {open && (
        <MenuPanel onClose={() => setOpen(false)} className="bottom-full right-0 z-20 mb-1 w-[240px]">
          {items.map((item) => (
            <MenuPanelItem
              key={item.label}
              onClick={() => {
                setOpen(false);
                item.onClick();
              }}
            >
              {item.label}
            </MenuPanelItem>
          ))}
        </MenuPanel>
      )}
    </div>
  );
}

export function ApprovalCard({
  approval,
  showSession = false,
  hotkey = false,
}: {
  approval: PendingApproval;
  showSession?: boolean;
  hotkey?: boolean;
}) {
  const resolveApproval = useStore((s) => s.resolveApproval);
  const session = useStore((s) => s.sessions.find((x) => isSession(x, { runnerId: approval.runnerId, pk: approval.sessionPk })));
  const [rejecting, setRejecting] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [answers, setAnswers] = useState<Record<string, string[]>>({});
  const [others, setOthers] = useState<Record<string, string>>({});
  const [step, setStep] = useState(0);

  const questions = useMemo<Question[]>(() => {
    if (approval.kind !== "question") return [];
    const raw = (approval.input as { questions?: Question[] } | null)?.questions;
    return Array.isArray(raw) ? raw : [];
  }, [approval]);

  // A new approval (new requestId) is a clean slate: reset the question
  // stepper, all picked answers/Other text, and the plan-rejection feedback
  // state so leftover input from a previous approval never leaks in.
  // biome-ignore lint/correctness/useExhaustiveDependencies: intentionally keyed on requestId only, to reset on a new approval
  useEffect(() => {
    setStep(0);
    setAnswers({});
    setOthers({});
    setFeedback("");
    setRejecting(false);
  }, [approval.requestId]);

  const resolve = (response: ApprovalResponse) => void resolveApproval(approval.runnerId, approval.requestId, response);

  const activeStep = questions.length === 0 ? 0 : Math.min(step, questions.length - 1);
  const activeQuestion = questions[activeStep];
  const isLastQuestion = activeStep >= questions.length - 1;

  const submitQuestions = () => {
    const merged: Record<string, string[]> = {};
    for (const q of questions) {
      const picked = answers[q.question] ?? [];
      const other = (others[q.question] ?? "").trim();
      merged[q.question] = other ? [...picked, other] : picked;
    }
    resolve({ decision: "allowOnce", scope: null, payload: { answers: merged } });
  };

  // Ref so the hotkey listener always calls the latest primary action without
  // resubscribing on every keystroke of the answers/feedback state.
  const primaryRef = useRef<() => void>(() => {});
  primaryRef.current = () => {
    if (approval.kind === "question") {
      if (questions.length === 0) return;
      if (isLastQuestion) submitQuestions();
      else setStep((s) => s + 1);
    } else if (approval.kind === "plan") {
      // While the reject-with-feedback panel is open, the hotkey must submit
      // that rejection — not approve the plan. Otherwise a user typing
      // feedback and pressing Ctrl/Cmd+Enter would invert their intent and
      // auto-approve edits.
      if (rejecting) resolve({ decision: "rejectOnce", scope: null, payload: { feedback } });
      else resolve({ decision: "allowOnce", scope: null, payload: { mode: "acceptEdits" } });
    } else resolve(once(true));
  };

  useEffect(() => {
    if (!hotkey) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        primaryRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hotkey]);

  const toggle = (q: Question, label: string) => {
    setAnswers((prev) => {
      const cur = prev[q.question] ?? [];
      const next = q.multiSelect ? (cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label]) : [label];
      return { ...prev, [q.question]: next };
    });
  };

  const title = approval.kind === "plan" ? "Plan review" : approval.kind === "question" ? "Question" : "Approval needed";

  return (
    <div className="mx-auto w-full max-w-[720px] overflow-hidden rounded-xl border border-border bg-card shadow-sm">
      <div className="flex items-center gap-2.5 border-b border-border bg-muted/40 px-3.5 py-2.5">
        <div className="flex h-[26px] w-[26px] items-center justify-center rounded-lg bg-amber-500/15 text-amber-600 dark:text-amber-400">
          <ShieldAlert size={14} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-[13px] font-semibold">{title}</div>
          <div className="flex min-w-0 items-center gap-1.5">
            <span className="truncate text-[11.5px] text-muted-foreground">{approval.tool}</span>
            {approval.principal && (
              <Pill variant="secondary" className="shrink-0">
                via {approval.principal.pluginName}
              </Pill>
            )}
          </div>
        </div>
        {showSession && session && (
          <Badge variant="secondary" className="max-w-[180px] truncate">
            {session.title ?? approval.sessionPk.slice(0, 8)}
          </Badge>
        )}
      </div>

      <div className="px-3.5 py-3">
        {approval.kind === "plan" ? (
          <div className="max-h-[360px] overflow-y-auto text-sm">
            <Markdown text={String((approval.input as { plan?: string } | null)?.plan ?? "")} />
          </div>
        ) : approval.kind === "question" ? (
          questions.length === 0 ? (
            <div className="text-[13px] text-muted-foreground">No questions were provided.</div>
          ) : (
            (() => {
              const q = activeQuestion;
              if (q === undefined) return null;
              return (
                <div className="space-y-3">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-[11.5px] text-muted-foreground">
                      Question {activeStep + 1} of {questions.length}
                    </span>
                    <div
                      role="progressbar"
                      aria-label="Question progress"
                      aria-valuenow={activeStep + 1}
                      aria-valuemin={1}
                      aria-valuemax={questions.length}
                      className="flex items-center gap-1"
                    >
                      {questions.map((question, i) => (
                        <span key={question.question} className={`h-1.5 w-4 rounded-full ${i <= activeStep ? "bg-primary" : "bg-muted"}`} />
                      ))}
                    </div>
                  </div>
                  <div key={q.question} className="space-y-1.5">
                    <div className="flex items-center gap-2">
                      <Badge variant="outline">{q.header}</Badge>
                      <h3 className="text-[13px] font-normal">{q.question}</h3>
                    </div>
                    <div className="space-y-1">
                      {q.options.map((o) => {
                        const selected = (answers[q.question] ?? []).includes(o.label);
                        return (
                          <Button
                            key={o.label}
                            variant={selected ? "secondary" : "ghost"}
                            aria-pressed={selected}
                            className="h-auto w-full items-start justify-start px-2.5 py-1.5 text-left"
                            onClick={() => toggle(q, o.label)}
                          >
                            <span className="mt-0.5 flex w-4 shrink-0 self-start justify-center">{selected && <Check size={13} />}</span>
                            <span className="min-w-0 flex-1">
                              <span className="block text-[12.5px] whitespace-normal break-words">{o.label}</span>
                              {o.description && (
                                <span className="block text-[11px] text-muted-foreground whitespace-normal break-words">
                                  {o.description}
                                </span>
                              )}
                            </span>
                          </Button>
                        );
                      })}
                      <Input
                        aria-label={`Other answer for ${q.header}`}
                        placeholder="Other…"
                        value={others[q.question] ?? ""}
                        onChange={(e) => setOthers((p) => ({ ...p, [q.question]: e.target.value }))}
                      />
                    </div>
                  </div>
                </div>
              );
            })()
          )
        ) : (
          <ToolBody approval={approval} />
        )}

        {rejecting && (
          <div className="mt-3">
            <Textarea
              aria-label="Feedback"
              placeholder="Why is this plan rejected? What should change?"
              value={feedback}
              onChange={(e) => setFeedback(e.target.value)}
            />
          </div>
        )}
      </div>

      <div className="flex flex-wrap justify-end gap-2 border-t border-border bg-muted/40 px-3.5 py-2.5">
        {approval.kind === "plan" ? (
          rejecting ? (
            <>
              <Button size="sm" variant="outline" onClick={() => setRejecting(false)}>
                Back
              </Button>
              <Button
                size="sm"
                variant="destructive"
                onClick={() => resolve({ decision: "rejectOnce", scope: null, payload: { feedback } })}
              >
                Send rejection
              </Button>
            </>
          ) : (
            <>
              <Button size="sm" variant="outline" onClick={() => setRejecting(true)}>
                Reject with feedback
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => resolve({ decision: "allowOnce", scope: null, payload: { mode: "default" } })}
              >
                Approve — review each edit
              </Button>
              <Button size="sm" onClick={() => resolve({ decision: "allowOnce", scope: null, payload: { mode: "acceptEdits" } })}>
                Approve — auto-approve edits
              </Button>
            </>
          )
        ) : approval.kind === "question" ? (
          <>
            <Button size="sm" variant="outline" onClick={() => resolve(once(false))}>
              Dismiss
            </Button>
            {questions.length > 0 && (
              <>
                {activeStep > 0 && (
                  <Button size="sm" variant="outline" onClick={() => setStep((s) => Math.max(s - 1, 0))}>
                    Back
                  </Button>
                )}
                {isLastQuestion ? (
                  <Button size="sm" onClick={submitQuestions}>
                    Submit
                  </Button>
                ) : (
                  <Button size="sm" onClick={() => setStep((s) => Math.min(s + 1, questions.length - 1))}>
                    Next
                  </Button>
                )}
              </>
            )}
          </>
        ) : (
          <>
            <ScopedAction
              label="Deny"
              menuLabel="Deny options"
              variant="outline"
              onPrimary={() => resolve(once(false))}
              items={[
                {
                  label: "Always deny in this project",
                  onClick: () => resolve({ decision: "rejectAlways", scope: "project", payload: null }),
                },
              ]}
            />
            <ScopedAction
              label="Allow"
              menuLabel="Allow options"
              onPrimary={() => resolve(once(true))}
              items={[
                {
                  label: "Allow for this session",
                  onClick: () => resolve({ decision: "allowAlways", scope: "session", payload: null }),
                },
                {
                  label: "Always allow in this project",
                  onClick: () => resolve({ decision: "allowAlways", scope: "project", payload: null }),
                },
              ]}
            />
          </>
        )}
      </div>
    </div>
  );
}
