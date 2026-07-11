import { FolderOpen } from "lucide-react";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { Button, Modal, ModalBody, ModalFooter, ModalHeader } from "@ryuzi/ui";

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
      <ModalHeader
        leading={<FolderOpen aria-hidden className="mt-0.5 size-4 text-muted-foreground" strokeWidth={2} />}
        title="Project settings"
        description={project.name}
      />
      <ModalBody>
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
      </ModalBody>
      <ModalFooter>
        <Button onClick={() => close(null)}>Done</Button>
      </ModalFooter>
    </Modal>
  );
}
