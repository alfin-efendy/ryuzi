import { Bot } from "lucide-react";
import { subagentSummaries } from "@/lib/subagents";
import { useStore } from "@/store";
import { sessKey } from "@/lib/session-key";

/** The RightPanel "Agents" tab body: the roster of sub-agents this session
 *  dispatched, derived live from its transcript rows. */
export function SubagentList({ runnerId, sessionPk }: { runnerId: string; sessionPk: string }) {
  const rows = useStore((s) => s.transcripts[sessKey(runnerId, sessionPk)]) ?? [];
  const agents = subagentSummaries(rows);
  if (agents.length === 0) {
    return <div className="p-4 text-[12.5px] text-muted-foreground">No sub-agents dispatched in this session.</div>;
  }
  return (
    <div className="flex flex-col gap-0.5 p-2">
      {agents.map((a) => (
        <div key={a.name} className="flex items-center gap-2 rounded-md px-2.5 py-1.5 text-[12.5px]">
          <Bot aria-hidden size={13} strokeWidth={2} className="size-[13px] shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate font-medium">{a.name}</span>
          {a.running && <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-primary" />}
          <span className="shrink-0 tabular-nums text-muted-foreground">
            {a.toolCount} {a.toolCount === 1 ? "call" : "calls"}
          </span>
        </div>
      ))}
    </div>
  );
}
