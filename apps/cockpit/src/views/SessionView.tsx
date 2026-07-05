import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowUp, ChevronDown, CircleAlert, FileText, GitBranch, Mic, PanelBottom, PanelRight, Paperclip, X } from "lucide-react";
import { toast } from "sonner";
import { Button, MenuPanel, MenuPanelItem as MenuItem, MenuPanelSection as MenuSectionLabel, Textarea } from "@ryuzi/ui";
import { commands } from "@/bindings";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { useNative } from "@/store-native";
import { chatRuntimeOf, useRuntimes } from "@/store-runtimes";
import { statusMeta } from "@/lib/status";
import { projectLabel } from "@/lib/sidebar";
import { basename } from "@/lib/paths";
import { activeContextQuery, replaceActiveContextToken, uniqueContextRefs } from "@/lib/composer-context";
import { composerMode } from "@/components/composerMode";
import { ApprovalPrompt } from "@/components/ApprovalPrompt";
import { StatusDot } from "@/components/common/bits";
import { Transcript } from "@/components/transcript/Transcript";
import { RightPanel } from "@/components/session/RightPanel";
import { BottomTerminalDrawer } from "@/components/session/BottomTerminalDrawer";
import { TodoPanel } from "@/components/session/TodoPanel";
import { startVoiceDictation } from "@/lib/voice";

export function SessionView() {
  const { sessions, transcripts, focusedSessionPk, send, stop, pendingApprovals, projects } = useStore();
  const nav = useNav();
  const [draft, setDraft] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [contextRefs, setContextRefs] = useState<string[]>([]);
  const [contextHits, setContextHits] = useState<string[]>([]);
  const [listening, setListening] = useState(false);
  const stopVoice = useRef<(() => void) | null>(null);

  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);
  const rows = (focusedSessionPk && transcripts[focusedSessionPk]) || [];
  const runtimes = useRuntimes((s) => s.runtimes);
  const project = projects.find((p) => p.projectId === session?.projectId);
  const runtimeId = project?.harness === "claude-code" ? "claude" : project?.harness;
  const agent = chatRuntimeOf(runtimes, runtimeId);
  const isNativeSession = runtimeId === "native";
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");
  const loadCommands = useNative((s) => s.loadCommands);
  const nativeCommands = useNative((s) => (project ? (s.commandsByProject[project.projectId] ?? []) : []));

  useEffect(() => {
    if (project && isNativeSession) void loadCommands(project.projectId);
  }, [project?.projectId, isNativeSession, loadCommands]);

  const slashQuery = useMemo(() => {
    const trimmed = draft.trimStart();
    if (!trimmed.startsWith("/") || trimmed.includes(" ")) return null;
    return trimmed.slice(1).toLowerCase();
  }, [draft]);
  const slashMatches = useMemo(() => {
    if (!isNativeSession || slashQuery === null) return [];
    return nativeCommands.filter((c) => c.name.toLowerCase().startsWith(slashQuery)).slice(0, 6);
  }, [isNativeSession, nativeCommands, slashQuery]);
  const contextQuery = useMemo(() => activeContextQuery(draft), [draft]);

  useEffect(() => {
    if (!project || contextQuery === null) {
      setContextHits([]);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      void commands.searchFiles(project.projectId, contextQuery.query).then((res) => {
        if (!cancelled) setContextHits(res.status === "ok" ? res.data.slice(0, 6) : []);
      });
    }, 120);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [project?.projectId, contextQuery?.query]);

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
    if (!t && attachments.length === 0) return;
    setDraft("");
    setAttachments([]);
    setContextRefs([]);
    void send(session.sessionPk, t, {
      context: { branch: session.branch, voiceTranscript: null, references: uniqueContextRefs(contextRefs) },
      attachments,
    });
  };

  const attachFiles = async () => {
    const picked = await commands.pickFiles();
    if (!picked.length) return;
    setAttachments((cur) => Array.from(new Set([...cur, ...picked])));
  };

  const pickContext = (path: string) => {
    setDraft((cur) => replaceActiveContextToken(cur, path));
    setContextRefs((cur) => uniqueContextRefs([...cur, path]));
    setContextHits([]);
  };

  const toggleVoice = () => {
    if (listening) {
      stopVoice.current?.();
      stopVoice.current = null;
      setListening(false);
      return;
    }
    const started = startVoiceDictation({
      onText: (text) => setDraft((cur) => (cur.trim() ? `${cur.trimEnd()} ${text}` : text)),
      onEnd: () => {
        stopVoice.current = null;
        setListening(false);
      },
      onError: (message) => toast.error(message),
    });
    if (!started.ok) {
      toast.error(started.message);
      return;
    }
    stopVoice.current = started.stop;
    setListening(true);
  };

  return (
    <div className="flex min-h-0 flex-1">
      {/* Chat column */}
      <div className={`flex min-h-0 min-w-0 flex-1 flex-col ${nav.rightMaximized && nav.rightOpen ? "hidden" : ""}`}>
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
          <Button
            variant="ghost"
            size="icon-sm"
            title="Toggle bottom panel"
            onClick={nav.toggleBottom}
            className={nav.bottomOpen ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
          >
            <PanelBottom aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            title="Toggle right panel"
            onClick={nav.toggleRight}
            className={nav.rightOpen ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
          >
            <PanelRight aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
          </Button>
        </div>

        {/* Native runtime plan (todowrite) */}
        <TodoPanel sessionPk={session.sessionPk} running={running} />

        {/* Transcript */}
        <Transcript rows={rows} agentName={agent?.name ?? "Agent"} agentColor={agent?.color ?? "var(--muted-foreground)"} running={running}>
          {hasApproval && <ApprovalPrompt sessionPk={session.sessionPk} />}
        </Transcript>

        {/* Session composer */}
        <div className="shrink-0 px-6 pb-4 pt-3">
          <div className="acrylic-card relative rounded-2xl border border-border shadow-xs">
            <Textarea
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
              className="field-sizing-fixed min-h-0 resize-none border-none bg-transparent px-4 pb-0.5 pt-[13px] text-[13.5px] leading-normal text-foreground focus-visible:ring-0 md:text-[13.5px] dark:bg-transparent"
            />
            {slashMatches.length > 0 && (
              <MenuPanel onClose={() => undefined} className="bottom-[82px] left-2.5 z-50 w-[320px]">
                <MenuSectionLabel>Commands</MenuSectionLabel>
                {slashMatches.map((cmd) => (
                  <MenuItem key={cmd.name} onClick={() => setDraft(`/${cmd.name} `)} className="font-medium">
                    <span className="font-mono text-[12px] text-muted-foreground">/{cmd.name}</span>
                    <span className="min-w-0 flex-1 truncate">{cmd.description}</span>
                  </MenuItem>
                ))}
              </MenuPanel>
            )}
            {contextHits.length > 0 && (
              <MenuPanel onClose={() => setContextHits([])} className="bottom-[82px] left-2.5 z-50 w-[360px]">
                <MenuSectionLabel>Context</MenuSectionLabel>
                {contextHits.map((path) => (
                  <MenuItem key={path} onClick={() => pickContext(path)} className="font-medium">
                    <FileText aria-hidden size={13} strokeWidth={2} className="size-[13px] text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate">{path}</span>
                  </MenuItem>
                ))}
              </MenuPanel>
            )}
            <div className="relative flex items-center gap-1.5 px-2.5 pb-2.5 pt-1.5">
              <Button variant="ghost" size="icon-sm" title="Attach" onClick={() => void attachFiles()} className="rounded-full text-muted-foreground">
                <Paperclip aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
              </Button>
              <Button variant="ghost" size="sm" className="font-medium" style={{ color: "#E8703A" }}>
                <CircleAlert aria-hidden size={12} strokeWidth={2} className="size-3" />
                Full access
                <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
              </Button>
              <div className="flex-1" />
              <Button variant="ghost" size="sm" className="font-semibold">
                <StatusDot color={agent?.color ?? "var(--muted-foreground)"} />
                {agent?.model || agent?.name || "No agent"}
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                title="Voice"
                onClick={toggleVoice}
                className={`rounded-full ${listening ? "bg-accent text-accent-foreground" : "text-muted-foreground"}`}
              >
                <Mic aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              </Button>
              {composerMode(session.status) === "stop" ? (
                <Button size="icon" title="Stop" onClick={() => void stop(session.sessionPk)} className="rounded-full">
                  <span className="h-[11px] w-[11px] rounded-[2px] bg-current" />
                </Button>
              ) : (
                <Button size="icon" title="Send" onClick={submit} className="rounded-full">
                  <ArrowUp aria-hidden size={14} strokeWidth={2.2} className="size-3.5" />
                </Button>
              )}
            </div>
            {(attachments.length > 0 || contextRefs.length > 0) && (
              <div className="flex flex-wrap gap-1.5 px-2.5 pb-2">
                {contextRefs.map((path) => (
                  <Button
                    key={`ctx-${path}`}
                    variant="outline"
                    size="sm"
                    title={path}
                    onClick={() => setContextRefs((cur) => cur.filter((p) => p !== path))}
                    className="max-w-[220px] rounded-full px-2 text-[12px] text-muted-foreground"
                  >
                    <FileText aria-hidden size={12} strokeWidth={2} className="size-3 shrink-0" />
                    <span className="truncate">{path}</span>
                    <X aria-hidden size={11} strokeWidth={2} className="size-[11px] shrink-0" />
                  </Button>
                ))}
                {attachments.map((path) => (
                  <Button
                    key={path}
                    variant="outline"
                    size="sm"
                    title={path}
                    onClick={() => setAttachments((cur) => cur.filter((p) => p !== path))}
                    className="max-w-[220px] rounded-full px-2 text-[12px] text-muted-foreground"
                  >
                    <Paperclip aria-hidden size={12} strokeWidth={2} className="size-3 shrink-0" />
                    <span className="truncate">{basename(path)}</span>
                    <X aria-hidden size={11} strokeWidth={2} className="size-[11px] shrink-0" />
                  </Button>
                ))}
              </div>
            )}
          </div>
        </div>

        {/* Bottom terminal drawer — a real shell in the session worktree */}
        {nav.bottomOpen && <BottomTerminalDrawer sessionPk={session.sessionPk} projectName={projectName} />}
      </div>

      {/* Right panel — keyed by session so switching sessions remounts it: per-session
          review/file state resets and in-flight gitDiff responses from the previous
          session land on an unmounted component instead of clobbering the new diff. */}
      {nav.rightOpen && (
        <RightPanel key={session.sessionPk} sessionPk={session.sessionPk} branch={session.branch ?? null} running={running} />
      )}
    </div>
  );
}
