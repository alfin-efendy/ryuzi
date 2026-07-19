import { useRef, useState } from "react";
import { Button, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import type { AgentDetailInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { DeleteAgentModal } from "./AgentActionsMenu";

export function AgentAdvancedTab({ detail, onDeleteSuccess }: { detail: AgentDetailInfo; onDeleteSuccess?: () => void }) {
  const saving = useAgents((state) => state.saving);
  const registry = useAgents((state) => state.registry);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const deleteTriggerRef = useRef<HTMLButtonElement>(null);

  const isDefault = registry?.defaultAgentId === detail.summary.id || detail.summary.isDefault;

  return (
    <div className="flex flex-col gap-3">
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
      <DeleteAgentModal
        agent={detail.summary}
        open={deleteOpen}
        trigger={deleteTriggerRef.current}
        onClose={() => setDeleteOpen(false)}
        onSuccess={onDeleteSuccess}
      />
    </div>
  );
}
