import { useEffect, useState } from "react";
import { CheckCircle2, ChevronDown, Circle, CircleDot, ListTodo } from "lucide-react";
import { Button, MenuPanel, MenuPanelSection } from "@ryuzi/ui";
import { useNative } from "@/store-native";
import type { TodoItem } from "@/bindings";

/** Collapsed-bar summary: done count, total, and the headline item — the first
 *  `in_progress` item, else the last completed one, else "Plan". */
export function todoBarSummary(todos: TodoItem[]): { done: number; total: number; label: string } {
  const done = todos.filter((t) => t.status === "completed").length;
  const active = todos.find((t) => t.status === "in_progress");
  const lastDone = [...todos].reverse().find((t) => t.status === "completed");
  return { done, total: todos.length, label: active?.content ?? lastDone?.content ?? "Plan" };
}

// Codex-style collapsed todo bar for the focused session, reflecting the native
// runtime's todowrite tool. One stable-height line (icon + done/total + current
// step); clicking it toggles a popover with the full list. Data reloads on
// mount/settle here, and live mid-run via the store's todowrite event trigger.
export function TodoPanel({ sessionPk, running }: { sessionPk: string; running: boolean }) {
  const todos = useNative((s) => s.todosBySession[sessionPk]);
  const loadTodos = useNative((s) => s.loadTodos);
  const [open, setOpen] = useState(false);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reload when the run settles
  useEffect(() => {
    void loadTodos(sessionPk);
  }, [sessionPk, running, loadTodos]);

  if (!todos || todos.length === 0) return null;
  const { done, total, label } = todoBarSummary(todos);

  return (
    <div className="relative shrink-0 border-b border-border px-3 py-1">
      <Button
        variant="ghost"
        size="sm"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="h-7 w-full justify-start gap-2 px-2 font-normal"
      >
        <ListTodo aria-hidden size={13} strokeWidth={2} className="size-[13px] shrink-0 text-muted-foreground" />
        <span className="shrink-0 font-semibold tabular-nums text-muted-foreground">{`${done}/${total}`}</span>
        <span className="min-w-0 flex-1 truncate text-left text-[12.5px]">{label}</span>
        <ChevronDown
          aria-hidden
          size={11}
          strokeWidth={2}
          className={`size-[11px] shrink-0 text-muted-foreground transition-transform ${open ? "rotate-180" : ""}`}
        />
      </Button>
      {open && (
        <MenuPanel onClose={() => setOpen(false)} className="left-3 top-full z-50 mt-1 w-[380px] max-w-[calc(100%-24px)]">
          <MenuPanelSection>{`Plan · ${done}/${total}`}</MenuPanelSection>
          <ul className="flex flex-col gap-0.5 px-2.5 pb-2">
            {todos.map((t, i) => (
              <li key={i} className="flex items-center gap-2 text-[12.5px]">
                <TodoIcon status={t.status} />
                <span className={t.status === "completed" ? "text-muted-foreground line-through" : ""}>{t.content}</span>
              </li>
            ))}
          </ul>
        </MenuPanel>
      )}
    </div>
  );
}

function TodoIcon({ status }: { status: string }) {
  if (status === "completed") return <CheckCircle2 aria-hidden size={13} className="shrink-0 text-green-500" />;
  if (status === "in_progress") return <CircleDot aria-hidden size={13} className="shrink-0 text-amber-500" />;
  return <Circle aria-hidden size={13} className="shrink-0 text-muted-foreground" />;
}
