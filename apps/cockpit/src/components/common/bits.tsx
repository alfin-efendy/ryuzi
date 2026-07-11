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
  variant?: "secondary" | "primary" | "warn" | "danger" | "mono";
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
  if (variant === "danger")
    return (
      <span className={cn(base, className)} style={{ background: "color-mix(in oklab, #EF4444 18%, transparent)", color: "#EF4444" }}>
        {children}
      </span>
    );
  if (variant === "mono")
    return <span className={cn(base, "bg-secondary font-mono font-normal text-secondary-foreground", className)}>{children}</span>;
  return <span className={cn(base, "bg-secondary text-secondary-foreground", className)}>{children}</span>;
}

// Red "Blocked" badge for a plugin the remote catalog's signed feed revoked
// (`PluginInfo.blockedReason` set) — the Browse card hides its Install
// button alongside this, and the Installed card shows it if a previously
// installed entry gets revoked later. Shares the `Pill` "danger" styling
// convention with `DoctorPanel`'s error-severity color.
export function BlockedBadge() {
  return <Pill variant="danger">Blocked</Pill>;
}

const CATEGORY_BADGES: Record<string, { label: string; color: string; outline?: boolean }> = {
  free: { label: "Free", color: "#22C55E" },
  free_tier: { label: "Free tier", color: "#22C55E", outline: true },
  oauth: { label: "OAuth", color: "#3B82F6" },
  api_key: { label: "API key", color: "#F59E0B" },
};

// Provider auth-category badge. `device` is an auth mechanism, not a pricing
// category — today's only device-flow provider (kiro) is free, so it renders
// as Free; the Add-account modal still says "Device sign-in".
export function CategoryBadge({ category, className }: { category: string; className?: string }) {
  const badge = CATEGORY_BADGES[category === "device" ? "free" : category];
  if (!badge) return null;
  const style: CSSProperties = badge.outline
    ? { color: badge.color, boxShadow: `inset 0 0 0 1px color-mix(in oklab, ${badge.color} 45%, transparent)` }
    : { background: `color-mix(in oklab, ${badge.color} 16%, transparent)`, color: badge.color };
  return (
    <span className={cn("rounded-full px-2 py-[2px] text-[10.5px] font-semibold tracking-[0.02em]", className)} style={style}>
      {badge.label}
    </span>
  );
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

const guideColor = "color-mix(in srgb, var(--sidebar-foreground) 20%, var(--sidebar))";

// Tree connector in front of session rows: a rounded elbow into the row, plus
// a vertical rail continuing to the next sibling. `reach` extends the lines
// past the row edges to bridge the 1px gaps between rows.
export function TreeGuide({ tail, reach }: { tail: boolean; reach: number }) {
  return (
    <span aria-hidden className="relative w-6 shrink-0 self-stretch">
      <span
        className="absolute left-3.5 box-border w-[9px] rounded-bl-[7px]"
        style={{
          top: -reach,
          height: `calc(50% + ${reach}px)`,
          borderLeft: `1.5px solid ${guideColor}`,
          borderBottom: `1.5px solid ${guideColor}`,
        }}
      />
      {tail && (
        <span
          className="absolute left-3.5 box-border w-[9px]"
          style={{ top: -reach, bottom: -reach, borderLeft: `1.5px solid ${guideColor}` }}
        />
      )}
    </span>
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
