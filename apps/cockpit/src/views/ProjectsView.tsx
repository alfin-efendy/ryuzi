import { useMemo, useState } from "react";
import { Folder, FolderPlus, Pencil, Plus, Search } from "lucide-react";
import { Button, Input } from "@ryuzi/ui";
import { useStore } from "@/store";
import { useUi } from "@/store-ui";
import { useNav } from "@/store-nav";
import { orderProjects, projectLabel, projectUpdatedAt, relativeShort } from "@/lib/sidebar";
import { AddProjectModal } from "@/components/modals/AddProjectModal";

/** Full-page project browser reached from the sidebar's "Projects" nav entry
 *  (shown in By-Task mode, where the sidebar no longer nests a Projects tree).
 *  Reuses the store's project list and the same New-task / settings / add
 *  affordances the sidebar rows carry, so behaviour never drifts. */
export function ProjectsView() {
  const { projects, sessions, selectProject } = useStore();
  const { projectOrdering, projectOrder } = useUi();
  const nav = useNav();
  const [query, setQuery] = useState("");
  const [addOpen, setAddOpen] = useState(false);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return orderProjects(projects, projectOrdering, projectOrder)
      .filter((p) => !q || projectLabel(p).toLowerCase().includes(q))
      .map((p) => ({ project: p, updated: projectUpdatedAt(p, sessions) }));
  }, [projects, sessions, projectOrdering, projectOrder, query]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Projects</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Repositories Cockpit works in. Open one to see its tasks or start a new one.
            </p>
          </div>
          <Button onClick={() => setAddOpen(true)} className="shrink-0">
            <FolderPlus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            New project
          </Button>
        </div>

        <div className="relative mb-4">
          <Search
            aria-hidden
            size={15}
            strokeWidth={2}
            className="pointer-events-none absolute left-3 top-1/2 size-[15px] -translate-y-1/2 text-muted-foreground"
          />
          <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search projects" className="pl-9" />
        </div>

        <div className="flex items-center gap-3 border-b border-border px-3 pb-2 text-[11px] font-medium uppercase tracking-[0.04em] text-muted-foreground">
          <span className="min-w-0 flex-1">Name</span>
          <span className="w-[120px] shrink-0">Updated</span>
          <span className="w-[64px] shrink-0" />
        </div>

        <div className="flex flex-col">
          {rows.map(({ project, updated }) => (
            <div
              key={project.projectId}
              className="group flex items-center gap-3 rounded-md px-3 py-3 transition-colors duration-150 ease-out hover:bg-accent/60"
            >
              <span className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                <Folder aria-hidden size={17} strokeWidth={2} />
              </span>
              <Button
                type="button"
                variant="ghost"
                className="h-auto min-w-0 flex-1 justify-start p-0 text-left text-[13px] font-semibold text-foreground hover:bg-transparent"
                onClick={() => nav.setProjectSettingsFor(project.projectId)}
              >
                <span className="min-w-0 truncate">{projectLabel(project)}</span>
              </Button>
              <span className="w-[120px] shrink-0 font-mono text-[11.5px] text-muted-foreground">{relativeShort(updated)}</span>
              <span className="flex w-[64px] shrink-0 items-center justify-end gap-1">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  title="New task"
                  className="text-muted-foreground opacity-0 transition-opacity duration-150 group-hover:opacity-100"
                  onClick={() => {
                    selectProject(project.projectId);
                    nav.navigate({ kind: "home" });
                  }}
                >
                  <Plus aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  title="Project settings"
                  className="text-muted-foreground opacity-0 transition-opacity duration-150 group-hover:opacity-100"
                  onClick={() => nav.setProjectSettingsFor(project.projectId)}
                >
                  <Pencil aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
                </Button>
              </span>
            </div>
          ))}
          {rows.length === 0 && (
            <div className="py-10 text-center text-[13px] text-muted-foreground">
              {query.trim() ? "No projects match your search." : "No projects yet. Create one to get started."}
            </div>
          )}
        </div>
      </div>
      <AddProjectModal open={addOpen} onClose={() => setAddOpen(false)} />
    </div>
  );
}
