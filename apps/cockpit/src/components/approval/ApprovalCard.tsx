import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Input, MenuPanel, MenuPanelItem, Textarea } from "@ryuzi/ui";
import { Check, ChevronDown, ShieldAlert } from "lucide-react";
import { useStore, type PendingApproval } from "@/store";
import type { ApprovalResponse } from "@/bindings";
import { Markdown } from "@/components/transcript/Markdown";

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
  const input = (approval.input ?? {}) as Record<string, unknown>;
  if (approval.tool === "bash" && typeof input.command === "string") {
    return <pre className="overflow-x-auto rounded-md bg-muted/60 px-3 py-2 font-mono text-xs whitespace-pre-wrap">{input.command}</pre>;
  }
  if (approval.tool === "edit" && typeof input.old_string === "string") {
    return (
      <div className="space-y-2">
        <div className="font-mono text-[11px] text-muted-foreground">{String(input.file_path ?? "")}</div>
        <pre className="overflow-x-auto rounded-md border border-red-500/25 bg-red-500/10 px-3 py-2 font-mono text-xs whitespace-pre-wrap">
          {String(input.old_string)}
        </pre>
        <pre className="overflow-x-auto rounded-md border border-emerald-500/25 bg-emerald-500/10 px-3 py-2 font-mono text-xs whitespace-pre-wrap">
          {String(input.new_string ?? "")}
        </pre>
      </div>
    );
  }
  return (
    <div className="space-y-2">
      <div className="font-mono text-xs break-words whitespace-pre-wrap">{approval.summary}</div>
      {Object.keys(input).length > 0 && (
        <details className="text-xs">
          <summary className="cursor-pointer text-muted-foreground">Parameters</summary>
          <pre className="mt-1 overflow-x-auto rounded-md bg-muted/60 px-3 py-2 font-mono text-[11px]">
            {JSON.stringify(input, null, 2)}
          </pre>
        </details>
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
  const session = useStore((s) => s.sessions.find((x) => x.sessionPk === approval.sessionPk));
  const [rejecting, setRejecting] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [answers, setAnswers] = useState<Record<string, string[]>>({});
  const [others, setOthers] = useState<Record<string, string>>({});

  const questions = useMemo<Question[]>(() => {
    if (approval.kind !== "question") return [];
    const raw = (approval.input as { questions?: Question[] } | null)?.questions;
    return Array.isArray(raw) ? raw : [];
  }, [approval]);

  const resolve = (response: ApprovalResponse) => void resolveApproval(approval.requestId, response);

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
    if (approval.kind === "question") submitQuestions();
    else if (approval.kind === "plan") {
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
          <div className="truncate text-[11.5px] text-muted-foreground">{approval.tool}</div>
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
          <div className="space-y-4">
            {questions.map((q) => (
              <div key={q.question} className="space-y-1.5">
                <div className="flex items-center gap-2">
                  <Badge variant="outline">{q.header}</Badge>
                  <span className="text-[13px]">{q.question}</span>
                </div>
                <div className="space-y-1">
                  {q.options.map((o) => {
                    const selected = (answers[q.question] ?? []).includes(o.label);
                    return (
                      <Button
                        key={o.label}
                        variant={selected ? "secondary" : "ghost"}
                        className="h-auto w-full justify-start px-2.5 py-1.5 text-left"
                        onClick={() => toggle(q, o.label)}
                      >
                        <span className="flex w-4 shrink-0 justify-center">{selected && <Check size={13} />}</span>
                        <span className="min-w-0">
                          <span className="block text-[12.5px]">{o.label}</span>
                          {o.description && <span className="block text-[11px] text-muted-foreground">{o.description}</span>}
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
            ))}
          </div>
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
            <Button size="sm" onClick={submitQuestions}>
              Submit
            </Button>
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
