import type { UsagePoint } from "@/bindings";

// Dependency-free bar chart: total tokens (input + output) per day.
export function UsageChart({ points }: { points: UsagePoint[] }) {
  if (points.length === 0) {
    return <div className="py-4 text-center text-xs text-muted-foreground">No usage recorded yet.</div>;
  }
  const totals = points.map((p) => p.inputTokens + p.outputTokens);
  const max = Math.max(1, ...totals);
  return (
    <div className="flex h-24 items-end gap-1">
      {points.map((p, i) => {
        const t = totals[i];
        const h = Math.round((t / max) * 100);
        return (
          <div
            key={p.day}
            className="flex flex-1 flex-col items-center gap-1"
            title={`${p.day}: ${t.toLocaleString()} tokens, ${p.requests} req`}
          >
            <div className="w-full rounded-sm bg-primary/70" style={{ height: `${Math.max(2, h)}%` }} />
            <span className="text-[9px] text-muted-foreground">{p.day.slice(5)}</span>
          </div>
        );
      })}
    </div>
  );
}
