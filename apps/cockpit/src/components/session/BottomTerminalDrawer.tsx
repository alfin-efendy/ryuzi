import { SquareTerminal, X } from "lucide-react";
import { useNav } from "@/store-nav";
import { TerminalPane } from "@/components/TerminalPane";

const toolBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

export function BottomTerminalDrawer({ sessionPk, projectName }: { sessionPk: string; projectName: string }) {
  const nav = useNav();
  return (
    <div className="acrylic-panel flex h-60 shrink-0 flex-col border-t border-border">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3.5 py-2">
        <SquareTerminal aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
        <span className="text-[12.5px] font-semibold">Terminal</span>
        <span className="font-mono text-[11px] text-muted-foreground">{projectName}</span>
        <div className="flex-1" />
        <button type="button" title="Close" onClick={nav.toggleBottom} className={`${toolBtn} h-[26px] w-[26px]`}>
          <X aria-hidden size={13} strokeWidth={2} />
        </button>
      </div>
      <TerminalPane sessionPk={sessionPk} className="flex-1" />
    </div>
  );
}
