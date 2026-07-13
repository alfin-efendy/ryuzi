import { useEffect, useRef, useState } from "react";
import { Button, Input, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import type { AgentDetailInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { DeleteAgentModal } from "./AgentActionsMenu";
import { mutationFromDetail } from "./agentMutation";

export function positiveLimit(raw: string): number | null {
  if (!/^\d+$/.test(raw.trim())) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value >= 1 ? value : null;
}

export function AgentAdvancedTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const registry = useAgents((state) => state.registry);
  const [maxTurns, setMaxTurns] = useState(String(detail.maxTurns));
  const [maxToolRounds, setMaxToolRounds] = useState(String(detail.maxToolRounds));
  const [deleteOpen, setDeleteOpen] = useState(false);
  const deleteTriggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    setMaxTurns(String(detail.maxTurns));
    setMaxToolRounds(String(detail.maxToolRounds));
  }, [detail]);

  const turns = positiveLimit(maxTurns);
  const rounds = positiveLimit(maxToolRounds);
  const isDefault = registry?.defaultAgentId === detail.summary.id || detail.summary.isDefault;

  return (
    <div className="flex flex-col gap-3">
      <SettingsCard>
        <div className="border-b border-border px-[18px] py-3.5">
          <SettingsCardTitle>Limits</SettingsCardTitle>
        </div>
        <SettingsCardRow className="gap-4">
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium">Max turns</span>
            {turns === null ? <span className="block text-[11px] text-destructive">Max turns must be at least 1.</span> : null}
          </span>
          <Input
            aria-label="Max turns"
            inputMode="numeric"
            className="w-[130px]"
            value={maxTurns}
            disabled={saving}
            onChange={(event) => setMaxTurns(event.target.value)}
          />
        </SettingsCardRow>
        <SettingsCardRow className="gap-4">
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium">Max tool rounds</span>
            {rounds === null ? <span className="block text-[11px] text-destructive">Max tool rounds must be at least 1.</span> : null}
          </span>
          <Input
            aria-label="Max tool rounds"
            inputMode="numeric"
            className="w-[130px]"
            value={maxToolRounds}
            disabled={saving}
            onChange={(event) => setMaxToolRounds(event.target.value)}
          />
        </SettingsCardRow>
        <div className="flex justify-end border-t border-border px-[18px] py-3">
          <Button
            disabled={saving || turns === null || rounds === null}
            onClick={() => {
              if (turns === null || rounds === null) return;
              void useAgents.getState().update(detail.summary.id, {
                ...mutationFromDetail(detail),
                maxTurns: turns,
                maxToolRounds: rounds,
              });
            }}
          >
            Save limits
          </Button>
        </div>
      </SettingsCard>

      <SettingsCard>
        <div className="border-b border-border px-[18px] py-3.5">
          <SettingsCardTitle>Default agent</SettingsCardTitle>
        </div>
        <SettingsCardRow className="gap-4">
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">
            {isDefault ? "This is the default agent." : "Use this agent when no agent is selected explicitly."}
          </span>
          {!isDefault ? (
            <Button variant="outline" disabled={saving} onClick={() => void useAgents.getState().setDefault(detail.summary.id)}>
              Make default
            </Button>
          ) : null}
        </SettingsCardRow>
      </SettingsCard>

      <SettingsCard className="border-destructive/40">
        <div className="border-b border-destructive/30 px-[18px] py-3.5">
          <SettingsCardTitle>Danger zone</SettingsCardTitle>
        </div>
        <SettingsCardRow className="gap-4">
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">
            Permanently remove this agent's configuration and isolated knowledge.
          </span>
          <Button ref={deleteTriggerRef} variant="destructive" disabled={saving} onClick={() => setDeleteOpen(true)}>
            Delete {detail.summary.name}
          </Button>
        </SettingsCardRow>
      </SettingsCard>
      <DeleteAgentModal agent={detail.summary} open={deleteOpen} trigger={deleteTriggerRef.current} onClose={() => setDeleteOpen(false)} />
    </div>
  );
}
