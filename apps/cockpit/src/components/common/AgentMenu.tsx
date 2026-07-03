import { AGENT_IDS, AGENTS, type AgentId } from "@/fixtures";
import { MenuItem, MenuPanel, MenuSectionLabel } from "./MenuPanel";
import { StatusDot } from "./bits";

// "Run with" agent picker shared by the home and session composers.
export function AgentMenu({
  value,
  onPick,
  onClose,
  className,
}: {
  value: AgentId;
  onPick: (id: AgentId) => void;
  onClose: () => void;
  className?: string;
}) {
  return (
    <MenuPanel onClose={onClose} className={className ?? "bottom-11 right-[78px] z-40 w-[280px]"}>
      <MenuSectionLabel>Run with</MenuSectionLabel>
      {AGENT_IDS.map((id) => {
        const a = AGENTS[id];
        return (
          <MenuItem
            key={id}
            selected={value === id}
            onClick={() => {
              onPick(id);
              onClose();
            }}
          >
            <StatusDot color={a.color} size={9} />
            <span className="min-w-0 flex-1">
              <span className="block text-[13px] font-medium">{a.name}</span>
              <span className="block text-[11.5px] text-muted-foreground">{a.model}</span>
            </span>
          </MenuItem>
        );
      })}
    </MenuPanel>
  );
}
