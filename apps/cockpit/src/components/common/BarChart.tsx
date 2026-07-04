// Daily-usage bar chart for provider detail cards.
export function BarChart({ data, color, maxBarWidth = 30 }: { data: { day: string; tok: number }[]; color: string; maxBarWidth?: number }) {
  const max = Math.max(...data.map((d) => d.tok), 1);
  return (
    <div className="box-border flex h-[130px] items-end gap-2 px-[18px] pb-3 pt-3.5">
      {data.map((d) => (
        <div key={d.day} className="flex h-full flex-1 flex-col items-center justify-end gap-1.5">
          <div
            title={`${d.tok}M tokens`}
            className="w-full rounded-t"
            style={{
              maxWidth: maxBarWidth,
              height: `${Math.max(4, (d.tok / max) * 100)}%`,
              background: `color-mix(in oklab, ${color} ${d.day === "Today" ? 90 : 55}%, transparent)`,
            }}
          />
          <span className="text-[10px] text-muted-foreground">{d.day}</span>
        </div>
      ))}
    </div>
  );
}
