import { useRef, useState } from "react";
import { Copy, MessageCircle, MoreHorizontal, Trash2 } from "lucide-react";
import { Button, MenuPanel, MenuPanelItem } from "@ryuzi/ui";
import type { AgentSummaryInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { useNav } from "@/store-nav";
import { ConfirmActionModal } from "@/components/modals/ConfirmActionModal";

export function DeleteAgentModal({
  agent,
  open,
  trigger,
  onClose,
}: {
  agent: AgentSummaryInfo;
  open: boolean;
  trigger: HTMLElement | null;
  onClose: () => void;
}) {
  const registry = useAgents((s) => s.registry);
  const canDelete = (registry?.agents.length ?? 0) > 1;
  return (
    <ConfirmActionModal
      open={open}
      title={`Delete ${agent.name}?`}
      description="Configuration and isolated knowledge will be permanently removed. Historical sessions remain readable."
      confirmLabel="Delete agent"
      busyLabel="Deleting…"
      confirmDisabled={!canDelete}
      trigger={trigger}
      onClose={onClose}
      onConfirm={() => useAgents.getState().remove(agent.id)}
    />
  );
}

export function AgentActionsMenu({ agent }: { agent: AgentSummaryInfo }) {
  const [open, setOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const saving = useAgents((s) => s.saving);
  const nav = useNav();

  const duplicate = async () => {
    setOpen(false);
    const created = await useAgents.getState().duplicate(agent.id);
    if (created) nav.navigate({ kind: "agentDetail", agentId: created.summary.id });
  };

  return (
    <span className="relative shrink-0">
      <Button
        ref={triggerRef}
        type="button"
        variant="ghost"
        size="icon-sm"
        aria-label={`Actions for ${agent.name}`}
        aria-expanded={open}
        disabled={saving}
        onClick={() => setOpen((value) => !value)}
        className="text-muted-foreground"
      >
        <MoreHorizontal aria-hidden size={15} strokeWidth={2} />
      </Button>
      {open && (
        <MenuPanel onClose={() => setOpen(false)} className="right-0 top-8 z-[70] w-[168px]">
          <div data-testid="agent-actions-panel">
            <MenuPanelItem
              onClick={() => {
                setOpen(false);
                nav.openAgentChat(agent.id);
              }}
            >
              <MessageCircle aria-hidden size={14} strokeWidth={2} />
              Start chat
            </MenuPanelItem>
            <MenuPanelItem onClick={() => void duplicate()}>
              <Copy aria-hidden size={14} strokeWidth={2} />
              Duplicate
            </MenuPanelItem>
            <MenuPanelItem
              className="text-destructive hover:text-destructive"
              onClick={() => {
                setOpen(false);
                setDeleteOpen(true);
              }}
            >
              <Trash2 aria-hidden size={14} strokeWidth={2} />
              Delete
            </MenuPanelItem>
          </div>
        </MenuPanel>
      )}
      <DeleteAgentModal agent={agent} open={deleteOpen} trigger={triggerRef.current} onClose={() => setDeleteOpen(false)} />
    </span>
  );
}
