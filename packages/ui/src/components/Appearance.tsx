import { Settings } from "lucide-react";
import { cn } from "../lib/utils";
import { ACCENTS, useTheme, type Mode } from "../theme";
import { Menu, MenuTrigger, MenuContent } from "./ui/menu";

const MODES: { key: Mode; label: string }[] = [
  { key: "light", label: "Light" },
  { key: "dark", label: "Dark" },
  { key: "system", label: "System" },
];

export function Appearance({ triggerClassName }: { triggerClassName?: string } = {}) {
  const { mode, accent, setMode, setAccent, capability, transparency, setTransparency, systemAccentHex } = useTheme();
  const activeKey = typeof accent === "object" ? "" : accent;
  const customValue = typeof accent === "object" ? accent.custom : "#4f46e5";

  return (
    <Menu>
      <MenuTrigger
        aria-label="Appearance"
        className={cn(
          "flex h-[34px] w-[34px] items-center justify-center rounded-lg border border-border bg-background text-muted-foreground hover:bg-accent hover:text-foreground",
          triggerClassName,
        )}
      >
        <Settings className="h-4 w-4" />
      </MenuTrigger>
      <MenuContent align="end" className="w-56 p-3">
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
          {systemAccentHex && (
            <button
              type="button"
              aria-label="System accent"
              title="Follow Windows accent"
              onClick={() => setAccent("system")}
              className={cn(
                "h-5 w-5 rounded-full border border-border",
                accent === "system" ? "ring-2 ring-ring ring-offset-1 ring-offset-popover" : "",
              )}
              style={{ background: systemAccentHex }}
            />
          )}
        </div>
        {capability !== "none" && (
          <>
            <div className="mt-3 mb-1 text-xs font-medium text-muted-foreground">Transparency</div>
            <button
              type="button"
              role="switch"
              aria-checked={transparency}
              onClick={() => setTransparency(!transparency)}
              className={cn(
                "relative h-5 w-9 rounded-full transition-colors",
                transparency ? "bg-primary" : "bg-muted",
              )}
            >
              <span
                className={cn(
                  "absolute top-0.5 h-4 w-4 rounded-full bg-background shadow transition-[left]",
                  transparency ? "left-[18px]" : "left-0.5",
                )}
              />
            </button>
          </>
        )}
      </MenuContent>
    </Menu>
  );
}
