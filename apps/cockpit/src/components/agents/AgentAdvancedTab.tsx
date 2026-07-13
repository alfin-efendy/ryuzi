import { useEffect, useRef, useState } from "react";
import { Button, Input, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import type { AgentDetailInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { mutationFromDetail } from "./agentMutation";
import { DeleteAgentModal } from "./AgentActionsMenu";

function positive(raw: string): number | null {
  if (!/^\d+$/.test(raw.trim())) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value >= 1 ? value : null;
}

export function AgentAdvancedTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const deleteTrigger = useRef<HTMLButtonElement>(null);
  const [turns, setTurns] = useState(String(detail.maxTurns));
  const [rounds, setRounds] = useState(String(detail.maxToolRounds));
  useEffect(() => {
    setTurns(String(detail.maxTurns));
    setRounds(String(detail.maxToolRounds));
  }, [detail]);
  const maxTurns = positive(turns);
  const maxToolRounds = positive(rounds);
  return (
    <div className="flex flex-col gap-3">
      <SettingsCard>
        <div className="border-b border-border px-[18px] py-3">
          <SettingsCardTitle>Loop limits</SettingsCardTitle>
        </div>
        <SettingsCardRow className="gap-4">
          <span className="w-40 text-[13px] font-medium">Max turns</span>
          <Input aria-label="Max turns" inputMode="numeric" value={turns} onChange={(event) => setTurns(event.target.value)} />
          {maxTurns === null ? <span className="text-xs text-destructive">Max turns must be at least 1.</span> : null}
        </SettingsCardRow>
        <SettingsCardRow className="gap-4">
          <span className="w-40 text-[13px] font-medium">Max tool rounds</span>
          <Input aria-label="Max tool rounds" inputMode="numeric" value={rounds} onChange={(event) => setRounds(event.target.value)} />
          {maxToolRounds === null ? <span className="text-xs text-destructive">Must be at least 1.</span> : null}
        </SettingsCardRow>
        <div className="flex justify-end px-[18px] py-3">
          <Button
            disabled={saving || maxTurns === null || maxToolRounds === null}
            onClick={() =>
              maxTurns &&
              maxToolRounds &&
              void useAgents.getState().update(detail.summary.id, { ...mutationFromDetail(detail), maxTurns, maxToolRounds })
            }
          >
            Save limits
          </Button>
        </div>
      </SettingsCard>
      <SettingsCard>
        <SettingsCardRow>
          <span className="min-w-0 flex-1">
            <SettingsCardTitle>Default agent</SettingsCardTitle>
            <span className="mt-0.5 block text-[11px] text-muted-foreground">Use this agent for new chats by default.</span>
          </span>
          <Button
            variant="outline"
            disabled={detail.summary.isDefault || saving}
            onClick={() => void useAgents.getState().setDefault(detail.summary.id)}
          >
            {detail.summary.isDefault ? "Default" : "Make default"}
          </Button>
        </SettingsCardRow>
      </SettingsCard>
      <SettingsCard>
        <SettingsCardRow>
          <span className="min-w-0 flex-1">
            <SettingsCardTitle>Danger zone</SettingsCardTitle>
            <span className="mt-0.5 block text-[11px] text-muted-foreground">Delete this agent from the local registry.</span>
          </span>
          <Button ref={deleteTrigger} variant="destructive" disabled={saving} onClick={() => setDeleteOpen(true)}>
            Delete {detail.summary.name}
          </Button>
          <DeleteAgentModal agent={detail.summary} open={deleteOpen} trigger={deleteTrigger.current} onClose={() => setDeleteOpen(false)} />
        </SettingsCardRow>
      </SettingsCard>
    </div>
  );
}
