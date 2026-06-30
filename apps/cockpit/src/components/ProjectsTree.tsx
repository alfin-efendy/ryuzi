import { useStore } from "@/store";
import { Button, Appearance, Menu, MenuTrigger, MenuContent, MenuItem, MenuSeparator } from "@harness/ui";

const DOT: Record<string, string> = {
  running: "bg-blue-500",
  idle: "bg-zinc-400",
  interrupted: "bg-amber-500",
  ended: "bg-zinc-300",
};

export function ProjectsTree() {
  const { projects, sessions, focusedSessionPk, selectedProjectId, setFocused, selectProject, addProject, stop, end } = useStore();
  const running = sessions.filter((s) => s.status === "running").length;
  const idle = sessions.filter((s) => s.status === "idle").length;

  return (
    <div className="flex h-full flex-col bg-sidebar">
      <div className="flex items-center gap-1.5 p-2.5">
        <Button size="sm" variant="outline" className="flex-1 justify-center" onClick={() => addProject()}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M12 5v14M5 12h14" /></svg>
          Add project
        </Button>
        <Appearance />
      </div>

      <div className="flex-1 overflow-auto px-2 pb-2">
        <div className="px-2 pt-2.5 pb-1.5 text-[10.5px] font-semibold tracking-wider text-muted-foreground uppercase">Projects</div>
        {projects.map((p) => (
          <div key={p.projectId} className="mb-1.5">
            <button
              type="button"
              onClick={() => selectProject(p.projectId)}
              title="New session on this project"
              className={`flex w-full items-center gap-1.5 rounded-lg px-2 py-1.5 text-left text-[12.5px] font-semibold text-muted-foreground hover:bg-accent hover:text-foreground ${
                selectedProjectId === p.projectId && !focusedSessionPk ? "bg-accent text-foreground" : ""
              }`}
            >
              <span className="truncate">{p.name}</span>
              <span className="ml-auto text-muted-foreground">＋</span>
            </button>
            {sessions
              .filter((s) => s.projectId === p.projectId)
              .map((s) => {
                const activeRow = focusedSessionPk === s.sessionPk;
                return (
                  <div
                    key={s.sessionPk}
                    className={`group ml-2 flex items-center gap-2.5 rounded-lg border-l-2 px-2.5 py-1.5 text-sm ${
                      activeRow ? "border-primary bg-primary/10 font-medium" : "border-transparent hover:bg-accent"
                    }`}
                  >
                    <button type="button" onClick={() => setFocused(s.sessionPk)} className="flex min-w-0 flex-1 items-center gap-2.5 text-left">
                      <span className={`h-2 w-2 shrink-0 rounded-full ${DOT[s.status] ?? "bg-zinc-400"}`} />
                      <span className="truncate">{s.title ?? s.sessionPk.slice(0, 8)}</span>
                    </button>
                    <Menu>
                      <MenuTrigger
                        aria-label="Session actions"
                        className="flex h-[22px] w-[22px] shrink-0 items-center justify-center rounded-md text-muted-foreground opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-background aria-expanded:opacity-100"
                      >
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.6" /><circle cx="12" cy="12" r="1.6" /><circle cx="19" cy="12" r="1.6" /></svg>
                      </MenuTrigger>
                      <MenuContent align="end" className="min-w-40">
                        {s.status === "running" && <MenuItem onClick={() => stop(s.sessionPk)}>Stop</MenuItem>}
                        {s.status === "running" && <MenuSeparator />}
                        <MenuItem variant="destructive" onClick={() => end(s.sessionPk)}>End session</MenuItem>
                      </MenuContent>
                    </Menu>
                  </div>
                );
              })}
          </div>
        ))}
      </div>

      <div className="flex items-center gap-2 border-t border-border px-3.5 py-2.5 text-[11.5px] text-muted-foreground">
        <span className="h-2 w-2 rounded-full bg-blue-500" /> {running} running · {idle} idle
      </div>
    </div>
  );
}
