import { Check, CircleDot, HandHelping, Loader2, X } from "lucide-react";
import { Button } from "@ryuzi/ui";
import { useStore } from "@/store";
import { agentColor } from "@/lib/agent-color";
import { LOCAL_RUNNER } from "@/lib/session-key";

// One lucide glyph per orch task status. `running` spins; everything else is
// static — see the `animate-spin` guard below.
const STATUS_ICON: Record<string, typeof CircleDot> = {
  todo: CircleDot,
  ready: CircleDot,
  running: Loader2,
  done: Check,
  failed: X,
  cancelled: X,
  blocked: HandHelping,
};

/**
 * Pinned, horizontally-scrollable strip of chips — one per subtask under
 * `rootId` — mounted above the transcript for a home chat with a live
 * orchestration. Clicking a chip drills into that subtask's worker session.
 * Row height is fixed regardless of chip count/status so it never shifts the
 * transcript layout; overflow scrolls sideways instead of wrapping.
 *
 * Renders `@ryuzi/ui`'s `Button` (compact "xs" outline pills) rather than a
 * raw `<button>` — CLAUDE.md forbids raw form elements in Cockpit views.
 */
export function TaskStrip({ rootId }: { rootId: string }) {
  const tasks = useStore((s) => s.orchTasks[rootId]) ?? [];
  const setFocused = useStore((s) => s.setFocused);
  // The root task itself reports with `rootId: null` (it has no parent) —
  // only render its children as chips.
  const children = tasks.filter((t) => t.rootId);
  if (children.length === 0) return null;

  return (
    <div className="flex h-10 shrink-0 items-center gap-1.5 overflow-x-auto overflow-y-hidden border-b border-border/60 px-4">
      {children.map((t) => {
        const Icon = STATUS_ICON[t.status] ?? CircleDot;
        const color = agentColor(t.agent || "build");
        return (
          <Button
            key={t.id}
            variant="outline"
            size="xs"
            title={`${t.title || "Untitled task"} — ${t.status}`}
            disabled={!t.sessionPk}
            onClick={() => t.sessionPk && setFocused({ runnerId: LOCAL_RUNNER, pk: t.sessionPk })}
            className="shrink-0 gap-1.5 rounded-full px-2.5 font-normal text-muted-foreground"
          >
            <Icon
              aria-hidden
              size={12}
              strokeWidth={2}
              className={`size-3 shrink-0 ${t.status === "running" ? "animate-spin" : ""}`}
              style={{ color }}
            />
            <span className="max-w-[10rem] truncate">{t.title || "Untitled task"}</span>
          </Button>
        );
      })}
    </div>
  );
}
