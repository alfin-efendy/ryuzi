import { ChevronDown, FolderOpen } from "lucide-react";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { useFixtures } from "@/store-fixtures";
import { AGENTS } from "@/fixtures";
import { Modal } from "./Modal";

const field = "flex h-[34px] items-center rounded-md border border-input bg-background px-3 text-[13px]";
const btn = "h-8 cursor-pointer rounded-md border border-border bg-transparent px-3.5 font-sans text-[12.5px] font-medium hover:bg-accent";

export function ProjectSettingsModal() {
  const projectId = useNav((s) => s.projectSettingsFor);
  const close = useNav((s) => s.setProjectSettingsFor);
  const project = useStore((s) => s.projects.find((p) => p.projectId === projectId));
  const defaultAgent = useFixtures((s) => s.defaultAgent);
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
        <div className="flex gap-3">
          <div className="flex flex-1 flex-col gap-1.5">
            <span className="text-xs font-semibold">Default branch</span>
            <div className={`${field} gap-2 font-mono text-xs`}>
              main
              <ChevronDown aria-hidden size={11} strokeWidth={2} className="ml-auto text-muted-foreground" />
            </div>
          </div>
          <div className="flex flex-1 flex-col gap-1.5">
            <span className="text-xs font-semibold">Default agent</span>
            <div className={`${field} gap-2 text-[12.5px]`}>
              {AGENTS[defaultAgent].name}
              <ChevronDown aria-hidden size={11} strokeWidth={2} className="ml-auto text-muted-foreground" />
            </div>
          </div>
        </div>
      </div>

      <div className="mt-[22px] flex items-center gap-2">
        <button type="button" className={`${btn} text-destructive`}>
          Archive project
        </button>
        <div className="flex-1" />
        <button type="button" className={`${btn} text-foreground`} onClick={() => close(null)}>
          Cancel
        </button>
        <button
          type="button"
          className="h-8 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
          onClick={() => close(null)}
        >
          Done
        </button>
      </div>
    </Modal>
  );
}
