import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowUp, ChevronDown, CircleAlert, FileText, GitBranch, Mic, PanelBottom, PanelRight, Paperclip, X } from "lucide-react";
import { toast } from "sonner";
import { Button, Combobox, MenuPanel, MenuPanelItem as MenuItem, MenuPanelSection as MenuSectionLabel, Textarea } from "@ryuzi/ui";
import { commands } from "@/bindings";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { useDiff } from "@/store-diff";
import { useNative } from "@/store-native";
import { useConnections } from "@/store-connections";
import { runtimeById, useRuntimes } from "@/store-runtimes";
import { statusMeta } from "@/lib/status";
import { projectLabel } from "@/lib/sidebar";
import { headerAgentLine } from "@/lib/session-header";
import { groupModelOptions } from "@/lib/model-groups";
import { activeContextQuery, replaceActiveContextToken, uniqueContextRefs } from "@/lib/composer-context";
import { PERM_MODES, corePermToUi, uiPermToCore, type UiPermMode } from "@/constants";
import { composerMode } from "@/components/composerMode";
import { ApprovalPrompt } from "@/components/ApprovalPrompt";
import { StatusDot } from "@/components/common/bits";
import { Transcript } from "@/components/transcript/Transcript";
import { RightPanel } from "@/components/session/RightPanel";
import { BottomTerminalDrawer } from "@/components/session/BottomTerminalDrawer";
import { TodoPanel } from "@/components/session/TodoPanel";
import { OpenInMenu } from "@/components/session/OpenInMenu";
import { startVoiceDictation } from "@/lib/voice";
import { useComposerAttachments } from "@/components/composer/useComposerAttachments";
import { AttachmentChips } from "@/components/composer/AttachmentChips";

export function SessionView() {
  const { sessions, transcripts, focusedSessionPk, send, stop, pendingApprovals, projects, setProjectModel, setProjectPermMode } =
    useStore();
  const nav = useNav();
  const [draft, setDraft] = useState("");
  const composerFiles = useComposerAttachments();
  const [contextRefs, setContextRefs] = useState<string[]>([]);
  const [contextHits, setContextHits] = useState<string[]>([]);
  const [listening, setListening] = useState(false);
  const stopVoice = useRef<(() => void) | null>(null);

  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);
  const rows = (focusedSessionPk && transcripts[focusedSessionPk]) || [];
  const runtimes = useRuntimes((s) => s.runtimes);
  const project = projects.find((p) => p.projectId === session?.projectId);
  const projectId = project?.projectId;
  // Ryuzi-only: every session runs the native runtime. Tolerant by
  // construction — legacy rows still saying "claude-code" (restored DBs)
  // are simply treated as native.
  const agent = runtimeById(runtimes, "native");
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");
  const loadCommands = useNative((s) => s.loadCommands);
  const nativeCommands = useNative((s) => (project ? (s.commandsByProject[project.projectId] ?? []) : []));
  const catalog = useConnections((s) => s.catalog);
  const connections = useConnections((s) => s.connections);
  const connectionsLoaded = useConnections((s) => s.loaded);
  const hydrateConnections = useConnections((s) => s.hydrate);

  useEffect(() => {
    if (projectId) void loadCommands(projectId);
  }, [projectId, loadCommands]);

  useEffect(() => {
    if (!connectionsLoaded) void hydrateConnections();
  }, [connectionsLoaded, hydrateConnections]);

  // Refresh edit-card diff stats after every turn, independent of the right
  // panel (which only fetches while open/on its own "review" tab).
  const fetchDiff = useDiff((s) => s.fetch);
  const sessionRunning = session?.status === "running";
  const prevSessionRunning = useRef(sessionRunning);
  useEffect(() => {
    if (prevSessionRunning.current && !sessionRunning && session?.sessionPk) {
      void fetchDiff(session.sessionPk);
    }
    prevSessionRunning.current = sessionRunning;
  }, [sessionRunning, session?.sessionPk, fetchDiff]);

  const slashQuery = useMemo(() => {
    const trimmed = draft.trimStart();
    if (!trimmed.startsWith("/") || trimmed.includes(" ")) return null;
    return trimmed.slice(1).toLowerCase();
  }, [draft]);
  const slashMatches = useMemo(() => {
    if (slashQuery === null) return [];
    return nativeCommands.filter((c) => c.name.toLowerCase().startsWith(slashQuery)).slice(0, 6);
  }, [nativeCommands, slashQuery]);
  const contextQuery = useMemo(() => activeContextQuery(draft), [draft]);
  const contextQueryText = contextQuery?.query ?? null;

  useEffect(() => {
    if (!projectId || contextQueryText === null) {
      setContextHits([]);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      void commands.searchFiles(projectId, contextQueryText).then((res) => {
        if (!cancelled) setContextHits(res.status === "ok" ? res.data.slice(0, 6) : []);
      });
    }, 120);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [projectId, contextQueryText]);

  if (!session) {
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Select a session from the sidebar.</div>
    );
  }

  const meta = statusMeta(session.status);
  const running = session.status === "running";
  const hasApproval = pendingApprovals.some((a) => a.sessionPk === session.sessionPk);
  const permUi = corePermToUi(project?.permMode ?? "default");
  const permMeta = PERM_MODES.find((m) => m.id === permUi) ?? PERM_MODES[1];
  const selectedModel = project?.model || agent?.model || "";
  const modelOptions = agent?.models ?? [];

  const submit = () => {
    const t = draft.trim();
    if (!t && composerFiles.attachments.length === 0) return;
    setDraft("");
    composerFiles.clear();
    setContextRefs([]);
    void send(session.sessionPk, t, {
      context: { branch: session.branch, voiceTranscript: null, references: uniqueContextRefs(contextRefs) },
      attachments: composerFiles.attachments,
    });
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
              <span>{headerAgentLine(agent, project)}</span>
              {session.branch && (
                <span className="inline-flex items-center gap-1">
                  <GitBranch aria-hidden size={11} strokeWidth={2} />
                  {session.branch}
                </span>
              )}
            </div>
          </div>
          <div className="flex-1" />
          <OpenInMenu sessionPk={session.sessionPk} />
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
        <Transcript
          sessionPk={session.sessionPk}
          rows={rows}
          agentName={agent?.name ?? "Agent"}
          agentColor={agent?.color ?? "var(--muted-foreground)"}
          running={running}
        >
          {hasApproval && <ApprovalPrompt sessionPk={session.sessionPk} />}
        </Transcript>

        {/* Session composer */}
        <div className="shrink-0 px-6 pb-4 pt-3">
          <div
            className={`acrylic-card relative mx-auto w-full max-w-3xl rounded-2xl border shadow-xs ${composerFiles.dragOver ? "border-primary" : "border-border"}`}
          >
            <Textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  submit();
                }
              }}
              onPaste={composerFiles.onPaste}
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
              <Button
                variant="ghost"
                size="icon-sm"
                title="Attach"
                onClick={() => void composerFiles.attachFiles()}
                className="rounded-full text-muted-foreground"
              >
                <Paperclip aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
              </Button>
              <Combobox
                aria-label="Permission mode"
                options={PERM_MODES.map((m) => ({ value: m.id, label: m.label, description: m.desc }))}
                value={permUi}
                onValueChange={(mode) => {
                  if (projectId) void setProjectPermMode(projectId, uiPermToCore(mode as UiPermMode));
                }}
                trigger={
                  <Button
                    variant="ghost"
                    size="sm"
                    title="Permission mode"
                    className="font-medium"
                    style={{ color: permUi === "full" ? "#E8703A" : undefined }}
                  >
                    <CircleAlert aria-hidden size={12} strokeWidth={2} className="size-3" />
                    {permMeta.label}
                    <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                  </Button>
                }
              />
              <div className="flex-1" />
              <Combobox
                aria-label="Model"
                options={groupModelOptions(modelOptions, catalog, connections)}
                value={selectedModel || null}
                onValueChange={(m) => {
                  if (projectId) void setProjectModel(projectId, m);
                }}
                disabled={modelOptions.length === 0}
                trigger={
                  <Button
                    variant="ghost"
                    size="sm"
                    title={modelOptions.length === 0 ? "No models available. Add a provider connection in Models." : "Model"}
                    className="font-semibold"
                  >
                    <StatusDot color={agent?.color ?? "var(--muted-foreground)"} />
                    {selectedModel || agent?.name || "No agent"}
                    <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                  </Button>
                }
              />
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
            {(composerFiles.attachments.length > 0 || contextRefs.length > 0) && (
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
                <AttachmentChips attachments={composerFiles.attachments} onRemove={composerFiles.remove} />
              </div>
            )}
          </div>
        </div>

        {/* Bottom terminal drawer — a real shell in the session worktree */}
        {nav.bottomOpen && <BottomTerminalDrawer sessionPk={session.sessionPk} projectName={projectName} />}
      </div>

      {/* Right panel — keyed by session so switching sessions remounts it: per-session
          review/file state resets, while diff data lives in the useDiff store keyed
          by sessionPk so sessions never see each other's results. */}
      {nav.rightOpen && (
        <RightPanel
          key={session.sessionPk}
          sessionPk={session.sessionPk}
          branch={session.branch ?? null}
          running={running}
          isGit={project?.isGit ?? true}
        />
      )}
    </div>
  );
}
