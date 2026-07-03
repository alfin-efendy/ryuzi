import { useMemo, useState } from "react";
import { ArrowUp, ChevronDown, CircleAlert, FolderOpen, GitBranch, Mic, Plus } from "lucide-react";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { HOME_SUGGESTIONS } from "@/constants";
import { agentById, defaultAgentOf, useAgents } from "@/store-agents";
import { projectLabel } from "@/lib/sidebar";
import { AgentMenu } from "@/components/common/AgentMenu";
import { MenuItem, MenuPanel, MenuSectionLabel, MenuSeparator } from "@/components/common/MenuPanel";
import { StatusDot } from "@/components/common/bits";

const roundBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-full border-none bg-transparent text-muted-foreground hover:bg-accent";
const chipBtn =
  "flex h-7 cursor-pointer items-center gap-[7px] rounded-md border-none bg-transparent px-2.5 font-sans text-[12.5px] hover:bg-accent";

export function HomeView() {
  const { projects, sessions, selectedProjectId, selectProject, start, addProject } = useStore();
  const nav = useNav();
  const [draft, setDraft] = useState("");
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  const [projectMenuOpen, setProjectMenuOpen] = useState(false);
  const [branchMenuOpen, setBranchMenuOpen] = useState(false);

  const project = projects.find((p) => p.projectId === selectedProjectId) ?? projects[0];
  const agents = useAgents((s) => s.agents);
  const agent = agentById(agents, nav.composerAgent) ?? defaultAgentOf(agents);

  const branches = useMemo(() => {
    const fromSessions = sessions.filter((s) => s.projectId === project?.projectId && s.branch).map((s) => s.branch as string);
    return [...new Set(["main", ...fromSessions])];
  }, [sessions, project?.projectId]);

  const send = async () => {
    const t = draft.trim();
    if (!t || !project) return;
    setDraft("");
    await start(project.projectId, t);
    nav.navigate({ kind: "session" });
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-7 p-10">
      <h1 className="m-0 text-center text-[30px] font-semibold tracking-[-0.02em]">
        What should we build{project ? ` in ${projectLabel(project)}` : ""}?
      </h1>
      <div className="w-full max-w-[720px]">
        <div className="acrylic-card relative rounded-2xl border border-border shadow-sm">
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            placeholder="Do anything"
            rows={2}
            className="box-border w-full resize-none border-none bg-transparent px-[18px] pb-1 pt-4 font-sans text-[14.5px] leading-normal text-foreground"
          />
          <div className="relative flex items-center gap-1.5 px-3 pb-3 pt-2">
            <button type="button" title="Attach" className={roundBtn}>
              <Plus aria-hidden size={16} strokeWidth={2} />
            </button>
            <button
              type="button"
              className="flex h-[30px] cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-2 font-sans text-[12.5px] font-medium hover:bg-accent"
              style={{ color: "#E8703A" }}
            >
              <CircleAlert aria-hidden size={13} strokeWidth={2} />
              Full access
              <ChevronDown aria-hidden size={12} strokeWidth={2} />
            </button>
            <div className="flex-1" />
            <button
              type="button"
              onClick={() => setAgentMenuOpen((v) => !v)}
              className="flex h-[30px] cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-2 font-sans text-[12.5px] font-semibold text-foreground hover:bg-accent"
            >
              <StatusDot color={agent?.color ?? "var(--muted-foreground)"} />
              {agent?.model || agent?.name || "No agent"}
              <span className="font-normal text-muted-foreground">{agent?.name ?? "install one"}</span>
              <ChevronDown aria-hidden size={12} strokeWidth={2} />
            </button>
            <button type="button" title="Voice" className={roundBtn}>
              <Mic aria-hidden size={14} strokeWidth={2} />
            </button>
            <button
              type="button"
              onClick={() => void send()}
              title="Start session"
              className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-full border-none bg-primary text-primary-foreground hover:opacity-85"
            >
              <ArrowUp aria-hidden size={15} strokeWidth={2.2} />
            </button>

            {agentMenuOpen && <AgentMenu value={nav.composerAgent} onPick={nav.setComposerAgent} onClose={() => setAgentMenuOpen(false)} />}
          </div>

          {/* Context chips */}
          <div className="relative flex items-center gap-1.5 border-t border-border px-3 py-2">
            <button type="button" onClick={() => setProjectMenuOpen((v) => !v)} className={`${chipBtn} font-semibold text-foreground`}>
              <FolderOpen aria-hidden size={13} strokeWidth={2} />
              {project ? projectLabel(project) : "No project"}
              <ChevronDown aria-hidden size={11} strokeWidth={2} />
            </button>
            <button
              type="button"
              onClick={() => setBranchMenuOpen((v) => !v)}
              className={`${chipBtn} font-medium text-muted-foreground hover:text-accent-foreground`}
            >
              <GitBranch aria-hidden size={13} strokeWidth={2} />
              {nav.composerBranch}
              <ChevronDown aria-hidden size={11} strokeWidth={2} />
            </button>

            {projectMenuOpen && (
              <MenuPanel onClose={() => setProjectMenuOpen(false)} className="left-3 top-[42px] z-50 w-60">
                <MenuSectionLabel>Project</MenuSectionLabel>
                {projects.map((p) => (
                  <MenuItem
                    key={p.projectId}
                    selected={p.projectId === project?.projectId}
                    onClick={() => {
                      selectProject(p.projectId);
                      setProjectMenuOpen(false);
                    }}
                    className="font-medium"
                  >
                    <FolderOpen aria-hidden size={13} strokeWidth={2} className="text-muted-foreground" />
                    <span className="flex-1">{projectLabel(p)}</span>
                  </MenuItem>
                ))}
                <MenuSeparator />
                <MenuItem
                  className="font-medium text-muted-foreground hover:text-accent-foreground"
                  onClick={() => {
                    setProjectMenuOpen(false);
                    void addProject();
                  }}
                >
                  <Plus aria-hidden size={13} strokeWidth={2} />
                  Open folder
                </MenuItem>
              </MenuPanel>
            )}

            {branchMenuOpen && (
              <MenuPanel onClose={() => setBranchMenuOpen(false)} className="left-60 top-[42px] z-50 w-[220px]">
                <MenuSectionLabel>Branch</MenuSectionLabel>
                {branches.map((b) => (
                  <MenuItem
                    key={b}
                    selected={b === nav.composerBranch}
                    onClick={() => {
                      nav.setComposerBranch(b);
                      setBranchMenuOpen(false);
                    }}
                    className="font-mono text-[12.5px]"
                  >
                    <GitBranch aria-hidden size={12} strokeWidth={2} className="text-muted-foreground" />
                    <span className="flex-1">{b}</span>
                  </MenuItem>
                ))}
              </MenuPanel>
            )}
          </div>
        </div>

        <div className="mt-4 flex flex-wrap justify-center gap-2">
          {HOME_SUGGESTIONS.map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => setDraft(s)}
              className="cursor-pointer rounded-full border border-border bg-transparent px-3 py-1.5 font-sans text-[12.5px] text-muted-foreground hover:bg-accent hover:text-accent-foreground"
            >
              {s}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
