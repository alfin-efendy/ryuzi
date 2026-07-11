import { useEffect, useRef } from "react";
import { CheckCircle2, ChevronDown, ChevronUp, Circle, CircleDot, ListTodo } from "lucide-react";
import { cn } from "@ryuzi/ui";
import { useNative } from "@/store-native";
import { sessKey } from "@/lib/session-key";
import type { TodoItem } from "@/bindings";

/** Step summary for the floating plan panel: `step` is the 1-based index of
 *  the first `in_progress` item (the footer's "Step X"); with no active item
 *  it rests at the completed count. `label` headlines the pill. */
export function todoStepSummary(todos: TodoItem[]): {
  step: number;
  total: number;
  done: number;
  label: string;
} {
  const done = todos.filter((t) => t.status === "completed").length;
  const activeIdx = todos.findIndex((t) => t.status === "in_progress");
  const active = activeIdx >= 0 ? todos[activeIdx] : undefined;
  const lastDone = [...todos].reverse().find((t) => t.status === "completed");
  return {
    step: activeIdx >= 0 ? activeIdx + 1 : done,
    total: todos.length,
    done,
    label: active?.content ?? lastDone?.content ?? "Plan",
  };
}

// Floating rounded plan panel (todowrite), overlaying the transcript above
// the composer — see docs/design/2026-07-10-cockpit-chat-batch3-design.md §1
// and the approved mockup. Expanded: header + step list + "Step X / N"
// footer. Collapsed: a pill with the live step summary. State per session.
export function TodoPanel({ runnerId, sessionPk, running }: { runnerId: string; sessionPk: string; running: boolean }) {
  const key = sessKey(runnerId, sessionPk);
  const todos = useNative((s) => s.todosBySession[key]);
  const loadTodos = useNative((s) => s.loadTodos);
  const collapsed = useNative((s) => s.planCollapsed[key] ?? false);
  const setCollapsed = useNative((s) => s.setPlanCollapsed);
  const activeRef = useRef<HTMLLIElement>(null);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reload when the run settles
  useEffect(() => {
    void loadTodos(runnerId, sessionPk);
  }, [runnerId, sessionPk, running, loadTodos]);

  const total = todos?.length ?? 0;
  const { step, done, label } = todoStepSummary(todos ?? []);
  const allDone = total > 0 && done === total;

  // Auto-collapse to the pill the moment the plan completes.
  useEffect(() => {
    if (allDone) setCollapsed(runnerId, sessionPk, true);
  }, [allDone, runnerId, sessionPk, setCollapsed]);

  // Keep the active step visible inside the internal scroll. (Optional call:
  // happy-dom elements don't implement scrollIntoView.)
  // biome-ignore lint/correctness/useExhaustiveDependencies: scrolls on step change only
  useEffect(() => {
    activeRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [step]);

  if (!todos || total === 0) return null;
  // The panel's job ends with the run: settled + fully complete → gone.
  if (!running && allDone) return null;

  return (
    <div className="pointer-events-none absolute bottom-3 left-1/2 z-20 flex w-[min(480px,calc(100%-32px))] -translate-x-1/2 flex-col items-center">
      {collapsed ? (
        <button
          type="button"
          aria-label="Expand plan"
          onClick={() => setCollapsed(runnerId, sessionPk, false)}
          className="acrylic-card pointer-events-auto flex max-w-full items-center gap-2 rounded-full border border-border px-3.5 py-1.5 text-[12.5px] shadow-lg"
        >
          {allDone ? (
            <CheckCircle2 aria-hidden size={13} className="shrink-0 text-green-500" />
          ) : (
            <CircleDot aria-hidden size={13} className="shrink-0 text-amber-500" />
          )}
          <span className="shrink-0 font-semibold tabular-nums text-muted-foreground">{`Step ${step}/${total}`}</span>
          <span className="min-w-0 truncate">{label}</span>
          <ChevronUp aria-hidden size={11} strokeWidth={2} className="size-[11px] shrink-0 text-muted-foreground" />
        </button>
      ) : (
        <div className="acrylic-card pointer-events-auto w-full overflow-hidden rounded-2xl border border-border shadow-lg">
          <button
            type="button"
            aria-label="Collapse plan"
            onClick={() => setCollapsed(runnerId, sessionPk, true)}
            className="flex w-full items-center gap-2 px-3.5 pb-1 pt-2.5 text-[11px] font-semibold uppercase tracking-[0.05em] text-muted-foreground"
          >
            <ListTodo aria-hidden size={12} strokeWidth={2} className="size-3 shrink-0" />
            Plan
            <ChevronDown aria-hidden size={11} strokeWidth={2} className="ml-auto size-[11px] shrink-0" />
          </button>
          <ul className="flex max-h-[45vh] flex-col gap-px overflow-y-auto px-2 pb-1.5">
            {todos.map((t, i) => {
              const active = t.status === "in_progress";
              return (
                <li
                  key={i}
                  ref={active ? activeRef : undefined}
                  className={cn(
                    "flex items-start gap-2.5 rounded-lg px-2 py-[7px] text-[12.5px] leading-[1.45]",
                    t.status === "completed" && "text-muted-foreground",
                    active && "bg-amber-500/10 font-medium",
                  )}
                >
                  <TodoIcon status={t.status} />
                  <span className="min-w-0">{t.content}</span>
                </li>
              );
            })}
          </ul>
          <div className="flex items-center justify-center gap-1.5 border-t border-border px-3 py-2 text-[12px] tabular-nums text-muted-foreground">
            <CircleDot aria-hidden size={12} className="size-3 shrink-0 text-amber-500" />
            Step <span className="font-semibold text-foreground">{step}</span> / {total}
          </div>
        </div>
      )}
    </div>
  );
}

function TodoIcon({ status }: { status: string }) {
  if (status === "completed") return <CheckCircle2 aria-hidden size={13} className="mt-0.5 shrink-0 text-green-500" />;
  if (status === "in_progress") return <CircleDot aria-hidden size={13} className="mt-0.5 shrink-0 text-amber-500" />;
  return <Circle aria-hidden size={13} className="mt-0.5 shrink-0 text-muted-foreground/70" />;
}
