import type { ReactNode } from "react";

import { cn } from "../../lib/utils";

// Translucent (acrylic) content card used by settings-family screens: a framed
// header row followed by label-left/control-right rows. Distinct from Card,
// which is the shadcn content card.
function SettingsCard({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("acrylic-card overflow-hidden rounded-xl border border-border shadow-xs", className)}>{children}</div>;
}

function SettingsCardHeader({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("flex items-center gap-2.5 border-b border-border px-[18px] py-[13px]", className)}>{children}</div>;
}

function SettingsCardTitle({ children }: { children: ReactNode }) {
  return <span className="text-[13.5px] font-semibold">{children}</span>;
}

function SettingsCardHint({ children }: { children: ReactNode }) {
  return <span className="text-xs text-muted-foreground">{children}</span>;
}

// Settings-style row inside a SettingsCard (label left, control right).
function SettingsCardRow({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("flex items-center gap-3 border-b border-border px-[18px] py-3 last:border-b-0", className)}>{children}</div>;
}

export { SettingsCard, SettingsCardHeader, SettingsCardTitle, SettingsCardHint, SettingsCardRow };
