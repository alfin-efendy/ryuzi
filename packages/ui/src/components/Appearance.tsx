import { useEffect, useRef, useState } from "react";
import { Settings } from "lucide-react";
import { cn } from "../lib/utils";
import { ACCENTS, useTheme, type Mode } from "../theme";

const MODES: { key: Mode; label: string }[] = [
  { key: "light", label: "Light" },
  { key: "dark", label: "Dark" },
  { key: "system", label: "System" },
];

export function Appearance() {
  const { mode, accent, setMode, setAccent } = useTheme();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const activeKey = typeof accent === "object" ? "" : accent;
  const customValue = typeof accent === "object" ? accent.custom : "#4f46e5";

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        aria-label="Appearance"
        onClick={() => setOpen((o) => !o)}
        className="rounded p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
      >
        <Settings className="h-4 w-4" />
      </button>
      {open && (
        <div className="absolute right-0 z-50 mt-1 w-56 rounded-md border border-border bg-popover p-3 text-popover-foreground shadow-md">
          <div className="mb-1 text-xs font-medium text-muted-foreground">Mode</div>
          <div className="mb-3 flex gap-1">
            {MODES.map((m) => (
              <button
                key={m.key}
                type="button"
                onClick={() => setMode(m.key)}
                className={cn(
                  "flex-1 rounded px-2 py-1 text-xs",
                  mode === m.key ? "bg-primary text-primary-foreground" : "bg-accent text-accent-foreground",
                )}
              >
                {m.label}
              </button>
            ))}
          </div>
          <div className="mb-1 text-xs font-medium text-muted-foreground">Accent</div>
          <div className="flex flex-wrap items-center gap-2">
            {ACCENTS.map((a) => (
              <button
                key={a.key}
                type="button"
                aria-label={a.label}
                title={a.label}
                onClick={() => setAccent(a.key)}
                className={cn(
                  "h-5 w-5 rounded-full border border-border",
                  activeKey === a.key ? "ring-2 ring-ring ring-offset-1 ring-offset-popover" : "",
                )}
                style={{ background: a.primary || "oklch(0.6 0 0)" }}
              />
            ))}
            <input
              type="color"
              aria-label="Custom accent"
              value={customValue}
              onChange={(e) => setAccent({ custom: e.target.value })}
              className="h-5 w-5 cursor-pointer rounded-full border border-border bg-transparent p-0"
            />
          </div>
        </div>
      )}
    </div>
  );
}
