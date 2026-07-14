import { MenuPanel, MenuPanelItem, MenuPanelSection } from "@ryuzi/ui";
import type { AgentSummaryInfo } from "@/bindings";

export function AgentMentionMenu({
  agents,
  activeIndex,
  onActiveIndexChange,
  onPick,
  onClose,
}: {
  agents: AgentSummaryInfo[];
  activeIndex: number;
  onActiveIndexChange: (index: number) => void;
  onPick: (agent: AgentSummaryInfo) => void;
  onClose?: () => void;
}) {
  const move = (offset: number) => {
    if (agents.length === 0) return;
    onActiveIndexChange((activeIndex + offset + agents.length) % agents.length);
  };

  return (
    <MenuPanel
      onClose={onClose ?? (() => undefined)}
      className="bottom-full left-3 z-50 mb-1.5 w-[320px]"
    >
      <div
        role="menu"
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") {
            event.preventDefault();
            move(1);
          } else if (event.key === "ArrowUp") {
            event.preventDefault();
            move(-1);
          } else if (event.key === "Enter" || event.key === "Tab") {
            const agent = agents[activeIndex];
            if (!agent) return;
            event.preventDefault();
            onPick(agent);
          }
        }}
      >
        <MenuPanelSection>Agents</MenuPanelSection>
        {agents.map((agent, index) => (
          <MenuPanelItem key={agent.id} selected={index === activeIndex} onClick={() => onPick(agent)} className="font-medium">
            <span className="size-2.5 shrink-0 rounded-full" style={{ backgroundColor: agent.avatarColor }} />
            <span className="min-w-0 flex-1 truncate">{agent.name}</span>
          </MenuPanelItem>
        ))}
      </div>
    </MenuPanel>
  );
}
