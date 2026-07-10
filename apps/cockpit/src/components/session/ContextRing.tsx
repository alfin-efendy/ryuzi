import { quotaColor } from "@/constants";

/** A small donut filling with context USED (calm→amber→red). `percentLeft`
 *  is the engine's "% context left"; the ring shows its complement. */
export function ContextRing({ percentLeft }: { percentLeft: number }) {
  const pctUsed = Math.max(0, Math.min(100, 100 - percentLeft));
  const size = 14;
  const stroke = 2.5;
  const r = (size - stroke) / 2;
  const circ = 2 * Math.PI * r;
  const offset = circ * (1 - pctUsed / 100);
  const color = quotaColor(pctUsed);
  return (
    <span className="flex items-center gap-1.5 text-[11px] tabular-nums text-muted-foreground">
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} className="shrink-0" aria-hidden>
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="currentColor" strokeWidth={stroke} className="text-border" />
        <circle
          data-ring="progress"
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke={color}
          strokeWidth={stroke}
          strokeDasharray={circ}
          strokeDashoffset={offset}
          strokeLinecap="round"
          transform={`rotate(-90 ${size / 2} ${size / 2})`}
        />
      </svg>
      {pctUsed}%
    </span>
  );
}
