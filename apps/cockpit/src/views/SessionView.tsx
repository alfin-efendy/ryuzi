import { useEffect, useRef, useState } from "react";
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
import { AGENTS, CODE_LINES, REVIEW_FILES, TERM_LINES, TREE_ITEMS, type DiffLine } from "@/fixtures";
import { statusMeta } from "@/lib/status";
import { basename } from "@/lib/paths";
import { projectLabel } from "@/lib/sidebar";
import { composerMode } from "@/components/composerMode";
import { ApprovalPrompt } from "@/components/ApprovalPrompt";
import { FileViewer } from "@/components/FileViewer";
import { AgentMenu } from "@/components/common/AgentMenu";
import { DiffStat, StatusDot } from "@/components/common/bits";

const toolBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

function diffLineStyle(l: DiffLine): { bg: string; numBg: string; sign: string; signColor: string; color: string } {
  const [type] = l;
  if (type === "hunk")
    return { bg: "var(--muted)", numBg: "transparent", sign: "⇅", signColor: "var(--muted-foreground)", color: "var(--muted-foreground)" };
  if (type === "add")
    return { bg: "rgba(34,197,94,0.12)", numBg: "rgba(34,197,94,0.14)", sign: "+", signColor: "#22C55E", color: "var(--foreground)" };
  if (type === "del")
    return { bg: "rgba(239,68,68,0.12)", numBg: "rgba(239,68,68,0.14)", sign: "−", signColor: "#EF4444", color: "var(--foreground)" };
  return { bg: "transparent", numBg: "transparent", sign: "", signColor: "transparent", color: "var(--code-foreground)" };
}

function Terminal({ className }: { className?: string }) {
  return (
    <div className={`flex min-h-0 flex-col overflow-auto px-4 py-3 font-mono text-xs leading-[1.75] ${className ?? ""}`}>
      {TERM_LINES.map((l, i) => (
        <div key={`${i}-${l.text}`} style={{ color: l.color }} className="whitespace-pre-wrap">
          {l.text || " "}
        </div>
      ))}
    </div>
  );
}

export function SessionView() {
  const { sessions, transcripts, focusedSessionPk, send, stop, pendingApprovals, projects } = useStore();
  const nav = useNav();
  const ui = useUi();
  const [draft, setDraft] = useState("");
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  const [reviewFile, setReviewFile] = useState(0);
  const [pathDraft, setPathDraft] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);

  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);
  // Persisted transcripts can carry empty text blocks (e.g. tool-only turns); skip them.
  const lines = ((focusedSessionPk && transcripts[focusedSessionPk]) || []).filter((l) => l.kind !== "text" || l.text.trim().length > 0);
  const agent = AGENTS[nav.composerAgent];
  const project = projects.find((p) => p.projectId === session?.projectId);
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");

  // biome-ignore lint/correctness/useExhaustiveDependencies(lines.length): re-run to pin the scroll to the bottom whenever the transcript grows
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [lines.length]);

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
    { id: "term", label: `pwsh in ${projectName}`, icon: SquareTerminal },
    { id: "file", label: activeFileTab ? activeFileTab.title : "Files", icon: FileText },
  ];

  const review = REVIEW_FILES[Math.min(reviewFile, REVIEW_FILES.length - 1)];
  const reviewAdd = REVIEW_FILES.reduce((n, f) => n + f.add, 0);
  const reviewDel = REVIEW_FILES.reduce((n, f) => n + f.del, 0);

  return (
    <div className="flex min-h-0 flex-1">
      {/* Chat column */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="flex shrink-0 items-center gap-3 border-b border-border px-5 py-3">
          <StatusDot color={meta.color} pulse={meta.pulse} size={9} />
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold tracking-[-0.01em]">{session.title || "Untitled session"}</div>
            <div className="flex items-center gap-2.5 text-xs text-muted-foreground">
              <span>
                {agent.name} · {agent.model}
              </span>
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
        <div ref={scrollRef} className="flex min-h-0 flex-1 flex-col gap-3.5 overflow-y-auto px-6 py-5">
          {lines.map((line, i) => {
            const key = `${i}-${line.kind}`;
            if (line.kind === "user")
              return (
                <div key={key} className="flex flex-col">
                  <div className="max-w-[70%] self-end rounded-xl bg-secondary px-3.5 py-2.5 text-[13.5px] leading-[1.55] text-secondary-foreground">
                    {line.text}
                  </div>
                </div>
              );
            if (line.kind === "status")
              return (
                <div key={key} className="flex flex-col">
                  <div className="acrylic-panel flex max-w-fit items-center gap-2 rounded-md border border-border px-3 py-[7px] font-mono text-xs text-muted-foreground">
                    <span style={{ color: "#22C55E" }}>›</span>
                    <span className="text-foreground">{line.text}</span>
                  </div>
                </div>
              );
            if (line.kind === "error")
              return (
                <div key={key} className="flex flex-col">
                  <div className="flex max-w-fit items-center gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-[7px] text-xs text-destructive">
                    {line.text}
                  </div>
                </div>
              );
            return (
              <div key={key} className="flex max-w-[82%] flex-col text-[13.5px] leading-relaxed text-foreground">
                <div className="mb-1 flex items-center gap-1.5 text-[11.5px] font-semibold text-muted-foreground">
                  <StatusDot color={agent.color} />
                  {agent.name}
                </div>
                <div className="whitespace-pre-wrap">{line.text}</div>
              </div>
            );
          })}
          {running && (
            <div className="flex items-center gap-2 text-[12.5px] text-muted-foreground">
              <span
                className="h-2 w-2 rounded-full"
                style={{ background: agent.color, animation: "relay-pulse 1.2s ease-in-out infinite" }}
              />
              Working…
            </div>
          )}
          {hasApproval && <ApprovalPrompt sessionPk={session.sessionPk} />}
        </div>

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
                <StatusDot color={agent.color} />
                {agent.model}
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

        {/* Bottom terminal drawer (design preview — no terminal backend yet) */}
        {nav.bottomOpen && (
          <div className="acrylic-panel flex h-60 shrink-0 flex-col border-t border-border">
            <div className="flex shrink-0 items-center gap-2 border-b border-border px-3.5 py-2">
              <SquareTerminal aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
              <span className="text-[12.5px] font-semibold">Terminal</span>
              <span className="font-mono text-[11px] text-muted-foreground">{projectName}</span>
              <div className="flex-1" />
              <button type="button" title="New terminal" className={`${toolBtn} h-[26px] w-[26px]`}>
                <Plus aria-hidden size={13} strokeWidth={2} />
              </button>
              <button type="button" title="Close" onClick={nav.toggleBottom} className={`${toolBtn} h-[26px] w-[26px]`}>
                <X aria-hidden size={13} strokeWidth={2} />
              </button>
            </div>
            <Terminal className="flex-1" />
          </div>
        )}
      </div>

      {/* Right panel */}
      {nav.rightOpen && (
        <div className="acrylic-panel flex w-[46%] max-w-[660px] shrink-0 flex-col border-l border-border">
          {/* Tab bar */}
          <div className="flex shrink-0 items-center gap-1 border-b border-border px-2.5 py-2">
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
            <button type="button" title="New tab" className={`${toolBtn} h-7 w-7`}>
              <Plus aria-hidden size={14} strokeWidth={2} />
            </button>
            <div className="flex-1" />
            <button type="button" title="Expand" className={`${toolBtn} h-7 w-7`}>
              <Maximize2 aria-hidden size={13} strokeWidth={2} />
            </button>
          </div>

          {/* Review tab (design preview — diff backend lands with specs 2–4) */}
          {nav.rightTab === "review" && (
            <>
              <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-4 py-2.5">
                <span className="font-mono text-xs text-muted-foreground">main → {session.branch ?? "worktree"}</span>
                <DiffStat add={reviewAdd} del={reviewDel} className="ml-auto" />
              </div>
              <div className="flex shrink-0 gap-1 overflow-x-auto border-b border-border px-3 py-2">
                {REVIEW_FILES.map((f, i) => (
                  <button
                    key={f.name}
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
              <div className="min-h-0 flex-1 overflow-auto py-2 font-mono text-xs leading-[1.7]">
                {review.lines.map((l, i) => {
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
              <div className="flex shrink-0 gap-2 border-t border-border px-4 py-3">
                <button
                  type="button"
                  className="h-8 cursor-pointer rounded-md border-none bg-primary px-4 font-sans text-[13px] font-medium text-primary-foreground hover:opacity-85"
                >
                  Accept all
                </button>
                <button
                  type="button"
                  className="h-8 cursor-pointer rounded-md border border-border bg-transparent px-4 font-sans text-[13px] font-medium text-foreground hover:bg-accent"
                >
                  Request changes
                </button>
                <div className="flex-1" />
                <button
                  type="button"
                  className="h-8 cursor-pointer rounded-md border border-border bg-transparent px-4 font-sans text-[13px] font-medium text-destructive hover:bg-accent"
                >
                  Discard
                </button>
              </div>
            </>
          )}

          {/* Terminal tab (design preview) */}
          {nav.rightTab === "term" && <Terminal className="flex-1 px-4 py-3.5" />}

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
                    <div className="flex flex-col py-2.5 font-mono leading-[1.75]">
                      {CODE_LINES.map(([text, color], i) => (
                        <div key={`${i}-${text}`} className="flex">
                          <span className="w-10 shrink-0 select-none pr-3.5 text-right text-[11px] text-code-number">{i + 1}</span>
                          <span className="whitespace-pre pr-4" style={{ color }}>
                            {text || " "}
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
                <div className="flex w-[200px] shrink-0 flex-col gap-2 overflow-y-auto border-l border-border p-2.5">
                  <div className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 text-xs text-muted-foreground [background:color-mix(in_oklab,var(--background)_45%,transparent)]">
                    <Search aria-hidden size={12} strokeWidth={2} />
                    Filter files
                  </div>
                  <div className="flex flex-col">
                    {TREE_ITEMS.map((t) => (
                      <div
                        key={`${t.depth}-${t.name}`}
                        className={`flex cursor-pointer items-center gap-1.5 rounded-sm py-[5px] pr-2 text-[12.5px] text-foreground hover:bg-accent ${t.sel ? "bg-accent" : ""}`}
                        style={{ paddingLeft: 8 + t.depth * 14 }}
                      >
                        {t.dir ? (
                          <ChevronRight
                            aria-hidden
                            size={11}
                            strokeWidth={2}
                            className="text-muted-foreground"
                            style={{ transform: t.open ? "rotate(90deg)" : undefined }}
                          />
                        ) : (
                          <FileText aria-hidden size={12} strokeWidth={2} className="text-muted-foreground" />
                        )}
                        {t.name}
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
