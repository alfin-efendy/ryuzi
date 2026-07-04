import { ChevronDown, FolderOpen } from "lucide-react";
import { useState } from "react";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { defaultRuntimeOf, useRuntimes } from "@/store-runtimes";
import { StatusDot } from "@/components/common/bits";
import { Button, MenuPanel, MenuPanelItem as MenuItem, Modal, ModalFooter } from "@ryuzi/ui";

const field = "flex h-[34px] items-center rounded-md border border-input bg-background px-3 text-[13px]";

export function ProjectSettingsModal() {
  const projectId = useNav((s) => s.projectSettingsFor);
  const close = useNav((s) => s.setProjectSettingsFor);
  const project = useStore((s) => s.projects.find((p) => p.projectId === projectId));
  const { runtimes, setDefault } = useRuntimes();
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  if (!projectId || !project) return null;
  const defaultAgent = defaultRuntimeOf(runtimes);
  const pickable = runtimes.filter((a) => a.enabled && a.binaryPath);
  return (
    <Modal onClose={() => close(null)} width={460}>
      <div className="mb-1 flex items-center gap-2.5">
        <FolderOpen aria-hidden size={16} strokeWidth={2} className="text-muted-foreground" />
        <span className="text-[15px] font-semibold tracking-[-0.01em]">Project settings</span>
      </div>
      <p className="mb-[18px] mt-0 text-[12.5px] text-muted-foreground">{project.name}</p>

      <div className="flex flex-col gap-3.5">
        <div className="flex flex-col gap-1.5">
          <span className="text-xs font-semibold">Name</span>
          <div className={field}>{project.name}</div>
        </div>
        <div className="flex flex-col gap-1.5">
          <span className="text-xs font-semibold">Local path</span>
          <div className={`${field} font-mono text-xs text-muted-foreground`}>{project.workdir}</div>
        </div>
        <div className="flex gap-3">
          <div className="flex flex-1 flex-col gap-1.5">
            <span className="text-xs font-semibold">Harness</span>
            <div className={`${field} font-mono text-xs text-muted-foreground`}>{project.harness}</div>
          </div>
          <div className="relative flex flex-1 flex-col gap-1.5">
            <span className="text-xs font-semibold">Default agent</span>
            <Button variant="outline" onClick={() => setAgentMenuOpen((v) => !v)} className="w-full justify-start gap-2 text-left">
              {defaultAgent?.name ?? "None detected"}
              <ChevronDown aria-hidden size={11} strokeWidth={2} className="ml-auto size-[11px] text-muted-foreground" />
            </Button>
            {agentMenuOpen && (
              <MenuPanel onClose={() => setAgentMenuOpen(false)} className="right-0 top-[60px] z-50 w-[220px]">
                {pickable.length === 0 && <div className="px-3 py-2 text-[12px] text-muted-foreground">No agents detected.</div>}
                {pickable.map((a) => (
                  <MenuItem
                    key={a.id}
                    selected={a.isDefault}
                    onClick={() => {
                      void setDefault(a.id);
                      setAgentMenuOpen(false);
                    }}
                  >
                    <StatusDot color={a.color} size={9} />
                    <span className="flex-1">{a.name}</span>
                  </MenuItem>
                ))}
              </MenuPanel>
            )}
          </div>
        </div>
      </div>

      <ModalFooter>
        <Button onClick={() => close(null)}>Done</Button>
      </ModalFooter>
    </Modal>
  );
}
