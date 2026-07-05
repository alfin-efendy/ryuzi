import { useEffect } from "react";
import { CheckCircle2, Circle, CircleDot } from "lucide-react";
import { useNative } from "@/store-native";

// A compact todo list for the focused session, reflecting the native runtime's
// todowrite tool. Reloads whenever the session settles (running -> idle).
export function TodoPanel({ sessionPk, running }: { sessionPk: string; running: boolean }) {
  const todos = useNative((s) => s.todosBySession[sessionPk]);
  const loadTodos = useNative((s) => s.loadTodos);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reload when the run settles
  useEffect(() => {
    void loadTodos(sessionPk);
  }, [sessionPk, running, loadTodos]);

  if (!todos || todos.length === 0) return null;
  const done = todos.filter((t) => t.status === "completed").length;

  return (
    <div className="shrink-0 border-b border-border px-5 py-2">
      <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
        Plan · {done}/{todos.length}
      </div>
      <ul className="flex flex-col gap-0.5">
        {todos.map((t, i) => (
          <li key={i} className="flex items-center gap-2 text-[12.5px]">
            <TodoIcon status={t.status} />
            <span className={t.status === "completed" ? "text-muted-foreground line-through" : ""}>{t.content}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function TodoIcon({ status }: { status: string }) {
  if (status === "completed") return <CheckCircle2 aria-hidden size={13} className="text-green-500" />;
  if (status === "in_progress") return <CircleDot aria-hidden size={13} className="text-amber-500" />;
  return <Circle aria-hidden size={13} className="text-muted-foreground" />;
}
