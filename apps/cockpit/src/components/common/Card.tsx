import type { ReactNode } from "react";
import { cn } from "@ryuzi/ui";

// Translucent content card used by every settings-family screen.
export function Card({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("acrylic-card overflow-hidden rounded-xl border border-border shadow-xs", className)}>{children}</div>;
}

export function CardHeader({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("flex items-center gap-2.5 border-b border-border px-[18px] py-[13px]", className)}>{children}</div>;
}

export function CardTitle({ children }: { children: ReactNode }) {
  return <span className="text-[13.5px] font-semibold">{children}</span>;
}

export function CardHint({ children }: { children: ReactNode }) {
  return <span className="text-xs text-muted-foreground">{children}</span>;
}

// Settings-style row inside a Card (label left, control right).
export function CardRow({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("flex items-center gap-3 border-b border-border px-[18px] py-3 last:border-b-0", className)}>{children}</div>;
}
