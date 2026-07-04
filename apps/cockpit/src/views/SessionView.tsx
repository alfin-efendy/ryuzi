import { useState } from "react";
import { ArrowUp, ChevronDown, CircleAlert, GitBranch, Mic, PanelBottom, PanelRight, Plus } from "lucide-react";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { runtimeById, defaultRuntimeOf, useRuntimes } from "@/store-runtimes";
import { statusMeta } from "@/lib/status";
import { projectLabel } from "@/lib/sidebar";
import { composerMode } from "@/components/composerMode";
import { ApprovalPrompt } from "@/components/ApprovalPrompt";
import { AgentMenu } from "@/components/common/AgentMenu";
import { StatusDot } from "@/components/common/bits";
import { Transcript } from "@/components/transcript/Transcript";
import { RightPanel } from "@/components/session/RightPanel";
import { BottomTerminalDrawer } from "@/components/session/BottomTerminalDrawer";

const toolBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

export function SessionView() {
  const { sessions, transcripts, focusedSessionPk, send, stop, pendingApprovals, projects } = useStore();
  const nav = useNav();
  const [draft, setDraft] = useState("");
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);

  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);
  const rows = (focusedSessionPk && transcripts[focusedSessionPk]) || [];
  const runtimes = useRuntimes((s) => s.runtimes);
  const agent = runtimeById(runtimes, nav.composerAgent) ?? defaultRuntimeOf(runtimes);
  const project = projects.find((p) => p.projectId === session?.projectId);
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");

  if (!session) {
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Select a session from the sidebar.</div>
    );
  }

  const meta = statusMeta(session.status);
  const running = session.status === "running";
  const hasApproval = pendingApprovals.some((a) => a.sessionPk === session.sessionPk);

  const submit = () => {
    const t = draft.trim();
    if (!t) return;
    setDraft("");
    void send(session.sessionPk, t);
  };

  return (
    <div className="flex min-h-0 flex-1">
      {/* Chat column */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="box-border flex h-[55px] shrink-0 items-center gap-3 border-b border-border px-5">
          <StatusDot color={meta.color} pulse={meta.pulse} size={9} />
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold tracking-[-0.01em]">{session.title || "Untitled session"}</div>
            <div className="flex items-center gap-2.5 text-xs text-muted-foreground">
              <span>{agent ? `${agent.name} · ${agent.model || agent.connection}` : "No agent detected"}</span>
              {session.branch && (
                <span className="inline-flex items-center gap-1">
                  <GitBranch aria-hidden size={11} strokeWidth={2} />
                  {session.branch}
                </span>
              )}
            </div>
          </div>
          <div className="flex-1" />
          <div className="mx-0.5 h-[18px] w-px bg-border" />
          <button
            type="button"
            title="Toggle bottom panel"
            onClick={nav.toggleBottom}
            className={`${toolBtn} ${nav.bottomOpen ? "bg-accent text-accent-foreground" : ""}`}
          >
            <PanelBottom aria-hidden size={15} strokeWidth={2} />
          </button>
          <button
            type="button"
            title="Toggle right panel"
            onClick={nav.toggleRight}
            className={`${toolBtn} ${nav.rightOpen ? "bg-accent text-accent-foreground" : ""}`}
          >
            <PanelRight aria-hidden size={15} strokeWidth={2} />
          </button>
        </div>

        {/* Transcript */}
        <Transcript rows={rows} agentName={agent?.name ?? "Agent"} agentColor={agent?.color ?? "var(--muted-foreground)"} running={running}>
          {hasApproval && <ApprovalPrompt sessionPk={session.sessionPk} />}
        </Transcript>

        {/* Session composer */}
        <div className="shrink-0 px-6 pb-4 pt-3">
          <div className="acrylic-card relative rounded-2xl border border-border shadow-xs">
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  submit();
                }
              }}
              placeholder="Ask for follow-up changes"
              rows={1}
              className="box-border w-full resize-none border-none bg-transparent px-4 pb-0.5 pt-[13px] font-sans text-[13.5px] leading-normal text-foreground"
            />
            <div className="relative flex items-center gap-1.5 px-2.5 pb-2.5 pt-1.5">
              <button
                type="button"
                title="Attach"
                className="flex h-7 w-7 cursor-pointer items-center justify-center rounded-full border-none bg-transparent text-muted-foreground hover:bg-accent"
              >
                <Plus aria-hidden size={15} strokeWidth={2} />
              </button>
              <button
                type="button"
                className="flex h-7 cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-2 font-sans text-xs font-medium hover:bg-accent"
                style={{ color: "#E8703A" }}
              >
                <CircleAlert aria-hidden size={12} strokeWidth={2} />
                Full access
                <ChevronDown aria-hidden size={11} strokeWidth={2} />
              </button>
              <div className="flex-1" />
              <button
                type="button"
                onClick={() => setAgentMenuOpen((v) => !v)}
                className="flex h-7 cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-2 font-sans text-xs font-semibold text-foreground hover:bg-accent"
              >
                <StatusDot color={agent?.color ?? "var(--muted-foreground)"} />
                {agent?.model || agent?.name || "No agent"}
                <ChevronDown aria-hidden size={11} strokeWidth={2} />
              </button>
              <button
                type="button"
                title="Voice"
                className="flex h-7 w-7 cursor-pointer items-center justify-center rounded-full border-none bg-transparent text-muted-foreground hover:bg-accent"
              >
                <Mic aria-hidden size={13} strokeWidth={2} />
              </button>
              {composerMode(session.status) === "stop" ? (
                <button
                  type="button"
                  title="Stop"
                  onClick={() => void stop(session.sessionPk)}
                  className="flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-full border-none bg-primary text-primary-foreground hover:opacity-85"
                >
                  <span className="h-[11px] w-[11px] rounded-[2px] bg-current" />
                </button>
              ) : (
                <button
                  type="button"
                  title="Send"
                  onClick={submit}
                  className="flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-full border-none bg-primary text-primary-foreground hover:opacity-85"
                >
                  <ArrowUp aria-hidden size={14} strokeWidth={2.2} />
                </button>
              )}
              {agentMenuOpen && (
                <AgentMenu
                  value={nav.composerAgent}
                  onPick={nav.setComposerAgent}
                  onClose={() => setAgentMenuOpen(false)}
                  className="bottom-[42px] right-[74px] z-40 w-[280px]"
                />
              )}
            </div>
          </div>
        </div>

        {/* Bottom terminal drawer — a real shell in the session worktree */}
        {nav.bottomOpen && <BottomTerminalDrawer sessionPk={session.sessionPk} projectName={projectName} />}
      </div>

      {/* Right panel */}
      {nav.rightOpen && <RightPanel sessionPk={session.sessionPk} branch={session.branch ?? null} running={running} />}
    </div>
  );
}
