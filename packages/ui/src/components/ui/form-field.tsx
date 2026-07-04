import type { ReactNode } from "react";

import { cn } from "../../lib/utils";

// Stacked label + control (+ optional hint) — the standard form row in modals
// and detail views. The wrapping <label> keeps the control focusable by click.
function FormField({ label, hint, className, children }: { label: ReactNode; hint?: ReactNode; className?: string; children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/noLabelWithoutControl: the control is always passed via `children`, which Biome cannot verify statically.
    <label className={cn("flex flex-col gap-1.5", className)}>
      <span className="text-xs font-semibold">{label}</span>
      {children}
      {hint ? <span className="text-xs text-muted-foreground">{hint}</span> : null}
    </label>
  );
}

export { FormField };
