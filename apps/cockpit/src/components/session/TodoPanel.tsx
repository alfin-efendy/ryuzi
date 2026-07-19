import { useEffect, useRef } from "react";
import { CheckCircle2, ChevronDown, ChevronUp, Circle, CircleDot, ListTodo } from "lucide-react";
import { Button, cn } from "@ryuzi/ui";
import { useNative } from "@/store-native";
import { sessKey } from "@/lib/session-key";
import type { TodoItem } from "@/bindings";

const TODO_LIST_LABEL = "TODO List";

export function todoStepSummary(todos: TodoItem[]): {
  step: number;
  total: number;
  done: number;
  label: string;
} {
  const done = todos.filter((todo) => todo.status === "completed").length;
  const activeIdx = todos.findIndex((todo) => todo.status === "in_progress");
  const active = activeIdx >= 0 ? todos[activeIdx] : undefined;
  const lastDone = [...todos].reverse().find((todo) => todo.status === "completed");
  return {
    step: activeIdx >= 0 ? activeIdx + 1 : done,
    total: todos.length,
    done,
    label: active?.content ?? lastDone?.content ?? "TODO List",
  };
}

/** A compact, session-scoped task board that stays at the top of the chat.
 * It expands for detail without displacing the transcript and collapses once
 * the work is done. */
export function TodoPanel({ runnerId, sessionPk, running }: { runnerId: string; sessionPk: string; running: boolean }) {
  const key = sessKey(runnerId, sessionPk);
  const todos = useNative((state) => state.todosBySession[key]);
  const loadTodos = useNative((state) => state.loadTodos);
  const collapsed = useNative((state) => state.planCollapsed[key] ?? false);
  const setCollapsed = useNative((state) => state.setPlanCollapsed);
  const activeRef = useRef<HTMLLIElement>(null);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reload when the run settles
  useEffect(() => {
    void loadTodos(runnerId, sessionPk);
  }, [runnerId, sessionPk, running, loadTodos]);

  const items = todos ?? [];
  const { step, total, done, label } = todoStepSummary(items);
  const allDone = total > 0 && done === total;
  const progress = total === 0 ? 0 : Math.round((done / total) * 100);

  useEffect(() => {
    if (allDone) setCollapsed(runnerId, sessionPk, true);
  }, [allDone, runnerId, sessionPk, setCollapsed]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll only when the active step changes
  useEffect(() => {
    activeRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [step]);

  if (total === 0) return null;

  return (
    <div className="pointer-events-none absolute left-1/2 top-3 z-20 flex w-[min(520px,calc(100%-32px))] -translate-x-1/2 flex-col items-center">
      {collapsed ? (
        <Button
          variant="outline"
          size="sm"
          aria-label="Expand TODO List"
          onClick={() => setCollapsed(runnerId, sessionPk, false)}
          className="pointer-events-auto h-8 max-w-full gap-2 rounded-full border-border bg-background px-3 shadow-lg"
        >
          {allDone ? (
            <CheckCircle2 aria-hidden size={13} className="shrink-0 text-green-500" />
          ) : (
            <CircleDot aria-hidden size={13} className="shrink-0 text-amber-500" />
          )}
          <span className="shrink-0 text-[12px] font-semibold tabular-nums text-muted-foreground">
            {done}/{total}
          </span>
          <span className="min-w-0 truncate text-[12px]">{allDone ? "All tasks completed" : label}</span>
          <ChevronUp aria-hidden size={12} className="shrink-0 text-muted-foreground" />
        </Button>
      ) : (
        <section
          aria-label={TODO_LIST_LABEL}
          className="pointer-events-auto w-full overflow-hidden rounded-xl border border-border bg-background shadow-lg"
        >
          <Button
            variant="ghost"
            size="sm"
            aria-label="Collapse TODO List"
            onClick={() => setCollapsed(runnerId, sessionPk, true)}
            className="h-auto w-full justify-start gap-2 rounded-none px-3.5 pb-2 pt-2.5 hover:bg-muted/60"
          >
            <div className="flex size-6 items-center justify-center rounded-md bg-amber-500/10 text-amber-600 dark:text-amber-400">
              <ListTodo aria-hidden size={13} strokeWidth={2} />
            </div>
            <span className="min-w-0 flex-1 text-left">
              <span className="block text-[12.5px] font-semibold">TODO List</span>
              <span className="block truncate text-[11px] font-normal text-muted-foreground">{label}</span>
            </span>
            <span className="text-[12px] font-medium tabular-nums text-muted-foreground">
              {done}/{total}
            </span>
            <ChevronDown aria-hidden size={12} className="shrink-0 text-muted-foreground" />
          </Button>
          <div
            className="mx-3.5 h-1 overflow-hidden rounded-full bg-muted"
            role="progressbar"
            aria-label="Task completion"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progress}
          >
            <div className="h-full rounded-full bg-amber-500 transition-[width]" style={{ width: `${progress}%` }} />
          </div>
          <ol className="max-h-[34vh] overflow-y-auto p-2" aria-label="Session tasks">
            {items.map((todo, index) => {
              const active = todo.status === "in_progress";
              return (
                <li
                  key={`${index}:${todo.content}`}
                  ref={active ? activeRef : undefined}
                  className={cn(
                    "flex items-start gap-2.5 rounded-lg border border-transparent px-2.5 py-2 text-[12.5px] leading-[1.45]",
                    todo.status === "completed" && "text-muted-foreground",
                    active && "border-amber-500/20 bg-amber-500/10",
                  )}
                >
                  <TodoIcon status={todo.status} />
                  <div className="min-w-0 flex-1">
                    <p className={cn("break-words", todo.status === "completed" && "line-through decoration-muted-foreground/50")}>
                      {todo.content}
                    </p>
                    {active && (
                      <span className="mt-1 block text-[10.5px] font-medium uppercase tracking-wide text-amber-600 dark:text-amber-400">
                        In progress
                      </span>
                    )}
                  </div>
                </li>
              );
            })}
          </ol>
          <div className="flex items-center gap-2 border-t border-border px-3.5 py-2 text-[11.5px] text-muted-foreground">
            {allDone ? (
              <CheckCircle2 aria-hidden size={13} className="text-green-500" />
            ) : (
              <CircleDot aria-hidden size={13} className="text-amber-500" />
            )}
            <span>{allDone ? "All tasks completed" : `Working on step ${step} of ${total}`}</span>
          </div>
        </section>
      )}
    </div>
  );
}

function TodoIcon({ status }: { status: string }) {
  if (status === "completed") return <CheckCircle2 aria-hidden size={14} className="mt-0.5 shrink-0 text-green-500" />;
  if (status === "in_progress") return <CircleDot aria-hidden size={14} className="mt-0.5 shrink-0 text-amber-500" />;
  return <Circle aria-hidden size={14} className="mt-0.5 shrink-0 text-muted-foreground/70" />;
}
