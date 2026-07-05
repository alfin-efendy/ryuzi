import type { CSSProperties, ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import { Badge, Button, cn } from "@ryuzi/ui";

// Muted icon avatar — the `Chip` sibling for rows identified by a lucide icon
// rather than a colored initial (plugin manifests only ever carry an icon
// name, never a brand color). Shared by the plugin detail header and the
// catalog tab's cards so both draw the exact same box.
export function IconChip({ icon: Icon, size = 36, className }: { icon: LucideIcon; size?: number; className?: string }) {
  return (
    <span
      className={cn(
        "flex shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground",
        size >= 44 && "rounded-lg",
        className,
      )}
      style={{ width: size, height: size }}
    >
      <Icon aria-hidden size={Math.round(size * 0.46)} strokeWidth={1.75} />
    </span>
  );
}

// One status Badge for a plugin: verified catalog/user entries read
// "Verified", entries the manifest itself flags as docs-only/at-risk read
// "Experimental" (and never get an enable toggle alongside it — see the
// plugin detail header and catalog tab), everything else is a community/
// user-authored plugin. Shared so the detail view and the catalog tab never
// disagree on which badge a given plugin gets.
export function PluginStatusBadge({ verified, experimental }: { verified: boolean; experimental: boolean }) {
  if (experimental) return <Badge variant="outline">Experimental</Badge>;
  if (verified) return <Badge>Verified</Badge>;
  return <Badge variant="secondary">Community</Badge>;
}

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
