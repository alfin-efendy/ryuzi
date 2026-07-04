import { useEffect, useState } from "react";
import {
  ArrowUp,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  FileText,
  GitBranch,
  Maximize2,
  Mic,
  PanelBottom,
  PanelRight,
  Plus,
  Search,
  SquareCheck,
  SquareTerminal,
  X,
} from "lucide-react";
import { useStore } from "@/store";
import { useUi } from "@/store-ui";
import { useNav, type RightTab } from "@/store-nav";
import { commands } from "@/bindings";
import { diffLineStyle, parseUnifiedDiff, type ReviewFile } from "@/lib/diff";
import { runtimeById, defaultRuntimeOf, useRuntimes } from "@/store-runtimes";
import { statusMeta } from "@/lib/status";
import { basename } from "@/lib/paths";
import { projectLabel } from "@/lib/sidebar";
import { composerMode } from "@/components/composerMode";
import { ApprovalPrompt } from "@/components/ApprovalPrompt";
import { FileViewer } from "@/components/FileViewer";
import { FileTreePane } from "@/components/FileTreePane";
import { TerminalPane } from "@/components/TerminalPane";
import { AgentMenu } from "@/components/common/AgentMenu";
import { DiffStat, StatusDot } from "@/components/common/bits";
import { Transcript } from "@/components/transcript/Transcript";

const toolBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

export function SessionView() {
  const { sessions, transcripts, focusedSessionPk, send, stop, pendingApprovals, projects } = useStore();
  const nav = useNav();
  const ui = useUi();
  const [draft, setDraft] = useState("");
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  const [reviewFile, setReviewFile] = useState(0);
  const [pathDraft, setPathDraft] = useState("");
  const [treeFilter, setTreeFilter] = useState("");
  const [reviewFiles, setReviewFiles] = useState<ReviewFile[]>([]);
  const [reviewLoading, setReviewLoading] = useState(false);
  const [reviewError, setReviewError] = useState<string | null>(null);

  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);
  const rows = (focusedSessionPk && transcripts[focusedSessionPk]) || [];
  const runtimes = useRuntimes((s) => s.runtimes);
  const agent = runtimeById(runtimes, nav.composerAgent) ?? defaultRuntimeOf(runtimes);
  const project = projects.find((p) => p.projectId === session?.projectId);
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");

  // Real diff of the session worktree; refreshed when the Review tab opens
  // and whenever the running turn finishes (the agent may have edited files).
  const sessionPk = session?.sessionPk;
  const running0 = session?.status === "running";
  useEffect(() => {
    if (!sessionPk || !nav.rightOpen || nav.rightTab !== "review" || running0) return;
    let cancelled = false;
    setReviewLoading(true);
    void commands.gitDiff(sessionPk).then((res) => {
      if (cancelled) return;
      setReviewLoading(false);
      if (res.status === "ok") {
        setReviewFiles(parseUnifiedDiff(res.data));
        setReviewError(null);
        setReviewFile(0);
      } else {
        setReviewError(res.error.message);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [sessionPk, nav.rightOpen, nav.rightTab, running0]);

  if (!session) {
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Select a session from the sidebar.</div>
    );
  }

  const meta = statusMeta(session.status);
  const running = session.status === "running";
  const hasApproval = pendingApprovals.some((a) => a.sessionPk === session.sessionPk);
  const activeFileTab = ui.tabs.find((t) => t.id === ui.activeTabId) ?? ui.tabs[0];

  const submit = () => {
    const t = draft.trim();
    if (!t) return;
    setDraft("");
    void send(session.sessionPk, t);
  };

  const rightTabs: { id: RightTab; label: string; icon: typeof SquareCheck }[] = [
    { id: "review", label: "Review", icon: SquareCheck },
    { id: "file", label: activeFileTab ? activeFileTab.title : "Files", icon: FileText },
  ];

  const review = reviewFiles.length > 0 ? reviewFiles[Math.min(reviewFile, reviewFiles.length - 1)] : null;
  const reviewAdd = reviewFiles.reduce((n, f) => n + f.add, 0);
  const reviewDel = reviewFiles.reduce((n, f) => n + f.del, 0);

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
        {nav.bottomOpen && (
          <div className="acrylic-panel flex h-60 shrink-0 flex-col border-t border-border">
            <div className="flex shrink-0 items-center gap-2 border-b border-border px-3.5 py-2">
              <SquareTerminal aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
              <span className="text-[12.5px] font-semibold">Terminal</span>
              <span className="font-mono text-[11px] text-muted-foreground">{projectName}</span>
              <div className="flex-1" />
              <button type="button" title="Close" onClick={nav.toggleBottom} className={`${toolBtn} h-[26px] w-[26px]`}>
                <X aria-hidden size={13} strokeWidth={2} />
              </button>
            </div>
            <TerminalPane sessionPk={session.sessionPk} className="flex-1" />
          </div>
        )}
      </div>

      {/* Right panel */}
      {nav.rightOpen && (
        <div className="acrylic-panel flex w-[46%] max-w-[660px] shrink-0 flex-col border-l border-border">
          {/* Tab bar */}
          <div className="box-border flex h-[55px] shrink-0 items-center gap-1 border-b border-border px-2.5">
            {rightTabs.map((t) => {
              const sel = nav.rightTab === t.id;
              const Icon = t.icon;
              return (
                <button
                  key={t.id}
                  type="button"
                  onClick={() => nav.setRightTab(t.id)}
                  className={`flex h-[30px] cursor-pointer items-center gap-[7px] whitespace-nowrap rounded-md border px-3 font-sans text-[12.5px] font-medium hover:bg-accent hover:text-accent-foreground ${
                    sel ? "border-border bg-background text-foreground" : "border-transparent bg-transparent text-muted-foreground"
                  }`}
                >
                  <Icon aria-hidden size={13} strokeWidth={2} />
                  {t.label}
                </button>
              );
            })}
            <div className="flex-1" />
            <button type="button" title="Expand" className={`${toolBtn} h-7 w-7`}>
              <Maximize2 aria-hidden size={13} strokeWidth={2} />
            </button>
          </div>

          {/* Review tab — the worktree's real git diff */}
          {nav.rightTab === "review" && (
            <>
              <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-4 py-2.5">
                <span className="font-mono text-xs text-muted-foreground">main → {session.branch ?? "worktree"}</span>
                <DiffStat add={reviewAdd} del={reviewDel} className="ml-auto" />
              </div>
              {reviewFiles.length > 0 && (
                <div className="flex shrink-0 gap-1 overflow-x-auto border-b border-border px-3 py-2">
                  {reviewFiles.map((f, i) => (
                    <button
                      key={`${f.dir}${f.name}`}
                      type="button"
                      onClick={() => setReviewFile(i)}
                      className={`flex h-7 cursor-pointer items-center gap-[7px] whitespace-nowrap rounded-md border px-2.5 font-mono text-[11.5px] text-foreground hover:bg-accent ${
                        i === reviewFile ? "border-border bg-background" : "border-transparent bg-transparent"
                      }`}
                    >
                      {f.name}
                      <DiffStat add={f.add} del={f.del} className="text-[11.5px]" />
                    </button>
                  ))}
                </div>
              )}
              <div className="min-h-0 flex-1 overflow-auto py-2 font-mono text-xs leading-[1.7]">
                {reviewError && <div className="px-4 py-3 text-[12.5px] text-destructive">{reviewError}</div>}
                {!reviewError && !review && (
                  <div className="px-4 py-3 font-sans text-[12.5px] text-muted-foreground">
                    {reviewLoading ? "Reading diff…" : "No changes in the worktree yet."}
                  </div>
                )}
                {review?.lines.map((l, i) => {
                  const s = diffLineStyle(l);
                  return (
                    <div key={`${i}-${l[2]}`} className="flex" style={{ background: s.bg, color: s.color }}>
                      <span
                        className="w-11 shrink-0 select-none pr-3 text-right text-[11px] text-code-number"
                        style={{ background: s.numBg }}
                      >
                        {l[1]}
                      </span>
                      <span className="w-4 shrink-0 select-none" style={{ color: s.signColor }}>
                        {s.sign}
                      </span>
                      <span className="whitespace-pre pr-4">{l[2]}</span>
                    </div>
                  );
                })}
              </div>
            </>
          )}

          {/* File tab — wired to the real readFile IPC via dock tabs */}
          {nav.rightTab === "file" && (
            <>
              <div className="flex shrink-0 items-center gap-1.5 border-b border-border px-4 py-2.5 text-xs text-muted-foreground">
                {activeFileTab ? (
                  <>
                    {activeFileTab.path
                      .split(/[\\/]/)
                      .slice(-3, -1)
                      .map((part) => (
                        <span key={part} className="flex items-center gap-1.5">
                          {part}
                          <ChevronRight aria-hidden size={11} strokeWidth={2} />
                        </span>
                      ))}
                    <span className="font-semibold text-foreground">{basename(activeFileTab.path)}</span>
                    <button
                      type="button"
                      title="Close file"
                      className={`${toolBtn} ml-auto h-6 w-6`}
                      onClick={() => ui.closeTab(activeFileTab.id)}
                    >
                      <X aria-hidden size={12} strokeWidth={2} />
                    </button>
                  </>
                ) : (
                  <form
                    className="flex h-7 w-full items-center gap-2 rounded-md border border-border px-2.5 [background:color-mix(in_oklab,var(--background)_45%,transparent)]"
                    onSubmit={(e) => {
                      e.preventDefault();
                      const p = pathDraft.trim();
                      if (p) ui.openFile(p);
                      setPathDraft("");
                    }}
                  >
                    <Search aria-hidden size={12} strokeWidth={2} />
                    <input
                      value={pathDraft}
                      onChange={(e) => setPathDraft(e.target.value)}
                      placeholder="Open file by absolute path"
                      className="flex-1 border-none bg-transparent font-mono text-[11.5px] text-foreground"
                    />
                  </form>
                )}
              </div>
              <div className="flex min-h-0 flex-1">
                <div className="flex min-w-0 flex-1 flex-col overflow-auto text-xs">
                  {activeFileTab ? (
                    <FileViewer path={activeFileTab.path} />
                  ) : (
                    <div className="flex flex-1 items-center justify-center font-sans text-[12.5px] text-muted-foreground">
                      Select a file from the tree.
                    </div>
                  )}
                </div>
                <div className="flex w-[200px] shrink-0 flex-col gap-2 overflow-y-auto border-l border-border p-2.5">
                  <div className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 text-xs text-muted-foreground [background:color-mix(in_oklab,var(--background)_45%,transparent)]">
                    <Search aria-hidden size={12} strokeWidth={2} />
                    <input
                      value={treeFilter}
                      onChange={(e) => setTreeFilter(e.target.value)}
                      placeholder="Filter files"
                      className="min-w-0 flex-1 border-none bg-transparent font-sans text-xs text-foreground outline-none"
                    />
                  </div>
                  <FileTreePane sessionPk={session.sessionPk} filter={treeFilter} />
                </div>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
