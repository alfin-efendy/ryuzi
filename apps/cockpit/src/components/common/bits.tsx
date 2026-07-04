import type { CSSProperties, ReactNode } from "react";
import { Button, cn } from "@ryuzi/ui";

// Tinted initial avatar used for providers, agents and apps.
export function Chip({
  initial,
  color,
  size = 36,
  mono = false,
  className,
  onClick,
}: {
  initial: string;
  color: string;
  size?: number;
  mono?: boolean;
  className?: string;
  onClick?: () => void;
}) {
  const style: CSSProperties = {
    width: size,
    height: size,
    color,
    background: `color-mix(in oklab, ${color} 15%, transparent)`,
    fontSize: Math.round(size * 0.4),
  };
  const classes = cn(
    "flex shrink-0 items-center justify-center rounded-md font-bold",
    mono && "font-mono font-semibold",
    size >= 44 && "rounded-lg",
    className,
  );
  if (onClick) {
    return (
      <Button variant="ghost" onClick={onClick} className={cn(classes, "h-auto cursor-pointer border-none p-0")} style={style}>
        {initial}
      </Button>
    );
  }
  return (
    <span className={classes} style={style}>
      {initial}
    </span>
  );
}

export function Pill({
  children,
  variant = "secondary",
  className,
}: {
  children: ReactNode;
  variant?: "secondary" | "primary" | "warn" | "mono";
  className?: string;
}) {
  const base = "rounded-full px-2 py-[2px] text-[10.5px] font-semibold tracking-[0.02em]";
  if (variant === "primary")
    return <span className={cn(base, "bg-primary uppercase tracking-[0.03em] text-primary-foreground", className)}>{children}</span>;
  if (variant === "warn")
    return (
      <span className={cn(base, className)} style={{ background: "color-mix(in oklab, #F59E0B 18%, transparent)", color: "#F59E0B" }}>
        {children}
      </span>
    );
  if (variant === "mono")
    return <span className={cn(base, "bg-secondary font-mono font-normal text-secondary-foreground", className)}>{children}</span>;
  return <span className={cn(base, "bg-secondary text-secondary-foreground", className)}>{children}</span>;
}

export function StatusDot({
  color,
  size = 7,
  pulse = false,
  className,
}: {
  color: string;
  size?: number;
  pulse?: boolean;
  className?: string;
}) {
  return (
    <span
      className={cn("shrink-0 rounded-full", className)}
      style={{ width: size, height: size, background: color, animation: pulse ? "relay-pulse 1.4s ease-in-out infinite" : undefined }}
    />
  );
}

export function QuotaTrack({ pct, color, height = 4 }: { pct: number; color: string; height?: number }) {
  return (
    <span className="block overflow-hidden rounded-full bg-muted" style={{ height }}>
      <span className="block h-full rounded-full" style={{ width: `${pct}%`, background: color }} />
    </span>
  );
}

export function DiffStat({ add, del, className }: { add: number; del: number; className?: string }) {
  return (
    <span className={cn("font-mono text-xs font-medium", className)}>
      <span style={{ color: "var(--diff-add-fg)" }}>+{add}</span> <span style={{ color: "var(--diff-del-fg)" }}>−{del}</span>
    </span>
  );
}
