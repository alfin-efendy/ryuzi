import { useStore } from "@/store";
import { Button } from "@/components/ui/button";

const DOT: Record<string, string> = {
  running: "bg-blue-500",
  idle: "bg-zinc-400",
  interrupted: "bg-amber-500",
  ended: "bg-zinc-300",
};

export function ProjectsTree() {
  const { projects, sessions, focusedSessionPk, selectedProjectId, setFocused, selectProject, addProject } = useStore();
  return (
    <div className="flex h-full flex-col gap-2 p-2">
      <Button size="sm" variant="secondary" onClick={() => addProject()}>+ Add project</Button>
      <div className="flex-1 overflow-auto">
        {projects.map((p) => (
          <div key={p.projectId} className="mb-2">
            <button
              onClick={() => selectProject(p.projectId)}
              title="New session on this project"
              className={`flex w-full items-center gap-1 rounded px-2 py-1 text-left text-xs font-semibold text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-100 ${
                selectedProjectId === p.projectId && !focusedSessionPk ? "bg-zinc-100 dark:bg-zinc-900" : ""
              }`}
            >
              <span className="truncate">{p.name}</span>
              <span className="ml-auto text-zinc-400">＋</span>
            </button>
            {sessions.filter((s) => s.projectId === p.projectId).map((s) => (
              <button
                key={s.sessionPk}
                onClick={() => setFocused(s.sessionPk)}
                className={`flex w-full items-center gap-2 rounded px-2 py-1 text-left text-sm ${
                  focusedSessionPk === s.sessionPk ? "bg-zinc-200 dark:bg-zinc-800" : ""
                }`}
              >
                <span className={`h-2 w-2 rounded-full ${DOT[s.status] ?? "bg-zinc-400"}`} />
                <span className="truncate">{s.title ?? s.sessionPk.slice(0, 8)}</span>
              </button>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
