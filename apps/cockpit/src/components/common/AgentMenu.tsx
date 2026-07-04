import { useRuntimes } from "@/store-runtimes";
import { MenuItem, MenuPanel, MenuSectionLabel } from "./MenuPanel";
import { StatusDot } from "./bits";

// "Run with" agent picker shared by the home and session composers. Lists
// enabled agents from the real catalog; agents without a session harness yet
// are shown disabled so the picker never promises what can't run.
export function AgentMenu({
  value,
  onPick,
  onClose,
  className,
}: {
  value: string;
  onPick: (id: string) => void;
  onClose: () => void;
  className?: string;
}) {
  const runtimes = useRuntimes((s) => s.runtimes);
  const visible = runtimes.filter((a) => a.enabled && a.binaryPath);
  return (
    <MenuPanel onClose={onClose} className={className ?? "bottom-11 right-[78px] z-40 w-[280px]"}>
      <MenuSectionLabel>Run with</MenuSectionLabel>
      {visible.length === 0 && (
        <div className="px-3 py-2 text-[12px] text-muted-foreground">
          No agents detected — install a CLI agent (e.g. Claude Code) and refresh in Runtime.
        </div>
      )}
      {visible.map((a) => (
        <MenuItem
          key={a.id}
          selected={value === a.id}
          onClick={() => {
            if (!a.runnable) return;
            onPick(a.id);
            onClose();
          }}
        >
          <StatusDot color={a.color} size={9} />
          <span className={`min-w-0 flex-1 ${a.runnable ? "" : "opacity-50"}`}>
            <span className="block text-[13px] font-medium">{a.name}</span>
            <span className="block text-[11.5px] text-muted-foreground">
              {a.runnable ? a.model || a.connection : "Session harness coming soon"}
            </span>
          </span>
        </MenuItem>
      ))}
    </MenuPanel>
  );
}
