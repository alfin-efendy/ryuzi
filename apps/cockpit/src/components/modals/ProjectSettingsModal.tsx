import { FolderOpen } from "lucide-react";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { Button, Modal, ModalFooter } from "@ryuzi/ui";

const field = "flex h-[34px] items-center rounded-md border border-input bg-background px-3 text-[13px]";

// Ryuzi-only sessions: there is no harness or default-agent choice anymore —
// every project runs the native runtime and models are picked per-composer.
export function ProjectSettingsModal() {
  const projectId = useNav((s) => s.projectSettingsFor);
  const close = useNav((s) => s.setProjectSettingsFor);
  const project = useStore((s) => s.projects.find((p) => p.projectId === projectId));
  if (!projectId || !project) return null;
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
      </div>

      <ModalFooter>
        <Button onClick={() => close(null)}>Done</Button>
      </ModalFooter>
    </Modal>
  );
}
