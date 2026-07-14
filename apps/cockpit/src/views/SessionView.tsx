import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowUp, ChevronDown, CircleAlert, FileText, GitBranch, Mic, PanelBottom, PanelRight, Paperclip, X } from "lucide-react";
import { toast } from "sonner";
import { Button, Combobox, MenuPanel, MenuPanelItem as MenuItem, MenuPanelSection as MenuSectionLabel, Textarea } from "@ryuzi/ui";
import { commands, type SessionRuntimeInfo } from "@/bindings";
import { useStore, type ChatOptions } from "@/store";
import { LOCAL_RUNNER, isSession, refKey } from "@/lib/session-key";
import { useNav } from "@/store-nav";
import { useDiff } from "@/store-diff";
import { useNative } from "@/store-native";
import { useConnections } from "@/store-connections";
import { useAgents } from "@/store-agents";
import { statusMeta } from "@/lib/status";
import { projectLabel } from "@/lib/sidebar";
import { headerAgentLine } from "@/lib/session-header";
import { sessionRuntimeScope } from "@/lib/session-runtime";
import { defaultAgentModel } from "@/lib/default-agent-model";
import { activeContextQuery, replaceActiveContextToken, uniqueContextRefs } from "@/lib/composer-context";
import { NATIVE_AGENT, PERM_MODES, corePermToUi, uiPermToCore, type UiPermMode } from "@/constants";
import { composerMode } from "@/components/composerMode";
import { ApprovalCard } from "@/components/approval/ApprovalCard";
import { StatusDot } from "@/components/common/bits";
import { ComposerModelEffortMenu } from "@/components/ComposerModelEffortMenu";
import { Transcript } from "@/components/transcript/Transcript";
import { TranscriptFileContext } from "@/components/transcript/TranscriptFileContext";
import { RightPanel } from "@/components/session/RightPanel";
import { BottomTerminalDrawer } from "@/components/session/BottomTerminalDrawer";
import { TodoPanel } from "@/components/session/TodoPanel";
import { TaskStrip } from "@/components/session/TaskStrip";
import { OpenInMenu } from "@/components/session/OpenInMenu";
import { SessionCostPanel } from "@/components/session/SessionCostPanel";
import { QueuedMessages } from "@/components/session/QueuedMessages";
import { startVoiceDictation } from "@/lib/voice";
import { useComposerAttachments } from "@/components/composer/useComposerAttachments";
import { AttachmentChips } from "@/components/composer/AttachmentChips";
import { HISTORY_IDLE, historyEntries, shouldNavigateHistory, stepHistory, type HistoryState } from "@/components/composer/inputHistory";

export function SessionView() {
  const {
    sessions,
    transcripts,
    focusedSession,
    send,
    stop,
    pendingApprovals,
    projects,
    setProjectRuntime,
    projectRuntimeById,
    loadProjectRuntime,
    sessionRuntimeById,
    loadSessionRuntime,
    setSessionPermMode,
    setSessionRuntime,
    orchTasks,
    loadOrchTasks,
  } = useStore();
  const enqueueMessage = useStore((s) => s.enqueueMessage);
  const nav = useNav();
  // Draft text lives in the persisted useNav drafts map keyed by session, so
  // switching sessions/views (SessionView renders un-keyed in App.tsx) swaps
  // the visible text instead of leaking one session's draft into another.
  const draftKey = focusedSession ? refKey(focusedSession) : "";
  const draft = nav.drafts[draftKey] ?? "";
  // Same call shape as the old useState setter so pickContext/voice callbacks
  // keep working; reads go through getState() to avoid stale closures.
  const setDraft = useCallback(
    (next: string | ((cur: string) => string)) => {
      const { drafts, setDraft: write } = useNav.getState();
      write(draftKey, typeof next === "function" ? next(drafts[draftKey] ?? "") : next);
    },
    [draftKey],
  );
  const session = sessions.find((s) => isSession(s, focusedSession));
  const runnerId = session?.runnerId ?? LOCAL_RUNNER;
  // A local ConPTY/bash and locally-installed apps can't operate on a remote
  // host's workdir — the bottom terminal drawer and Open-in menu are gated
  // off entirely for sessions running on a non-local runner.
  const isRemote = runnerId !== LOCAL_RUNNER;
  const composerFiles = useComposerAttachments(runnerId);
  const [contextRefs, setContextRefs] = useState<string[]>([]);
  const [contextHits, setContextHits] = useState<string[]>([]);
  const [listening, setListening] = useState(false);
  const stopVoice = useRef<(() => void) | null>(null);

  const rows = (focusedSession && transcripts[refKey(focusedSession)]) || [];
  // Ryuzi-only: every session runs the native agent. Tolerant by
  // construction — legacy rows still saying "claude-code" (restored DBs)
  // are simply treated as native.
  const agentModels = useAgents((s) => s.models);
  const agentModel = useAgents((s) => defaultAgentModel(s.registry));
  const project = projects.find((p) => p.projectId === session?.projectId);
  const projectId = project?.projectId;
  const runtimeScope = sessionRuntimeScope(session?.kind, projectId ?? null);
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");
  const loadCommands = useNative((s) => s.loadCommands);
  const nativeCommands = useNative((s) => (project ? (s.commandsByProject[project.projectId] ?? []) : []));
  const connectionsLoaded = useConnections((s) => s.loaded);
  const hydrateConnections = useConnections((s) => s.hydrate);

  useEffect(() => {
    // Slash commands are project metadata on the local engine.
    if (projectId) void loadCommands(LOCAL_RUNNER, projectId);
  }, [projectId, loadCommands]);

  useEffect(() => {
    if (projectId) void loadProjectRuntime(projectId);
  }, [projectId, loadProjectRuntime]);

  useEffect(() => {
    if (session?.sessionPk && runtimeScope === "session") void loadSessionRuntime(runnerId, session.sessionPk);
  }, [runtimeScope, runnerId, session?.sessionPk, loadSessionRuntime]);

  useEffect(() => {
    if (!connectionsLoaded) void hydrateConnections();
  }, [connectionsLoaded, hydrateConnections]);

  // Home chats with a live orchestration mount a task strip above the
  // transcript. `orch_list_roots` returns every root with no per-home filter,
  // so the home→root mapping is resolved client-side: once per focused
  // session, and again whenever the store's orch task graph grows a root this
  // component hasn't seen yet (a fresh orchTaskChanged for a new goal).
  // Re-resolve the home→root mapping whenever any orch task's status changes,
  // not only when a brand-new root appears. A root finishing is a same-key
  // in-place status update that leaves the root COUNT unchanged, so keying the
  // effect on the count alone left the strip mounted (showing a stale "live"
  // state) after a run completed. This signal changes on every status delta,
  // so the effect re-runs and the `!live` branch clears `orchRootForHome`.
  const orchStatusSignal = Object.values(orchTasks)
    .flat()
    .map((t) => `${t.id}:${t.status}`)
    .join("|");
  const [orchRootForHome, setOrchRootForHome] = useState<Record<string, string>>({});
  const homeSessionPk = session?.kind === "chat" ? session.sessionPk : null;

  // biome-ignore lint/correctness/useExhaustiveDependencies: orchStatusSignal is a deliberate re-run trigger (any orch status change), not read in the body
  useEffect(() => {
    if (!homeSessionPk) return;
    let alive = true;
    void commands.orchListRoots().then((res) => {
      if (!alive || res.status !== "ok") return;
      const live = res.data
        .filter((t) => t.homeSessionPk === homeSessionPk && ["decomposing", "waiting", "judging"].includes(t.status))
        .sort((a, b) => b.createdAt - a.createdAt)[0];
      setOrchRootForHome((cur) => {
        if (!live) {
          if (!(homeSessionPk in cur)) return cur;
          const next = { ...cur };
          delete next[homeSessionPk];
          return next;
        }
        return cur[homeSessionPk] === live.id ? cur : { ...cur, [homeSessionPk]: live.id };
      });
    });
    return () => {
      alive = false;
    };
  }, [homeSessionPk, orchStatusSignal]);

  const orchRootId = homeSessionPk ? orchRootForHome[homeSessionPk] : undefined;

  // A fresh strip on mount: seed the full task graph (titles, agents) once
  // the home→root mapping above resolves — orchTaskChanged only carries a
  // status delta, not the row's display fields.
  useEffect(() => {
    if (orchRootId) void loadOrchTasks(orchRootId);
  }, [orchRootId, loadOrchTasks]);

  // Refresh edit-card diff stats after every turn, independent of the right
  // panel (which only fetches while open/on its own "review" tab).
  const fetchDiff = useDiff((s) => s.fetch);
  const sessionRunning = session?.status === "running";
  const prevSessionRunning = useRef(sessionRunning);
  useEffect(() => {
    if (prevSessionRunning.current && !sessionRunning && session?.sessionPk) {
      void fetchDiff(runnerId, session.sessionPk);
    }
    prevSessionRunning.current = sessionRunning;
  }, [sessionRunning, session?.sessionPk, runnerId, fetchDiff]);

  // Session working directory, used to linkify workspace file paths in the
  // transcript's markdown (see TranscriptFileContext).
  const [workdir, setWorkdir] = useState<string | null>(null);
  useEffect(() => {
    setWorkdir(null);
    if (!session?.sessionPk) return;
    let alive = true;
    void commands.sessionWorkdir(runnerId, session.sessionPk).then((res) => {
      if (alive && res.status === "ok") setWorkdir(res.data);
    });
    return () => {
      alive = false;
    };
  }, [session?.sessionPk, runnerId]);
  // Provider value for TranscriptFileContext — memoized so the Transcript's
  // WorkspacePathCode instances don't all re-render on every SessionView render.
  const transcriptFileCtx = useMemo(
    () => (workdir && session?.sessionPk ? { runnerId, sessionPk: session.sessionPk, workdir } : null),
    [runnerId, session?.sessionPk, workdir],
  );

  // ArrowUp/Down history over this session's sent messages. A ref (not state)
  // holds the navigation cursor — it never drives rendering.
  const historyRef = useRef<HistoryState>(HISTORY_IDLE);
  const history = useMemo(() => historyEntries(rows), [rows]);
  useEffect(() => {
    historyRef.current = HISTORY_IDLE;
    return () => {
      // Leaving this session while history navigation is active (switching
      // sessions/views, or unmounting) would otherwise strand the
      // pre-recall draft in this in-memory ref forever — write it back to
      // the persisted drafts map so it survives. This cleanup runs on both
      // dependency change and unmount, covering both loss paths with one
      // code path. A completed send resets historyRef to HISTORY_IDLE
      // synchronously in submit() — without a session change — so that
      // path never reaches this cleanup with a stale pending value.
      if (historyRef.current.index >= 0) {
        useNav.getState().setDraft(draftKey, historyRef.current.pending);
      }
    };
  }, [draftKey]);

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
      void commands.searchFiles(LOCAL_RUNNER, projectId, contextQueryText).then((res) => {
        if (!cancelled) setContextHits(res.status === "ok" ? res.data.slice(0, 6) : []);
      });
    }, 120);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [projectId, contextQueryText]);

  const projectRuntime = projectId ? (projectRuntimeById[projectId] ?? null) : null;
  const agentModelInfo = agentModels.find((model) => model.requestValue === agentModel) ?? null;
  const readOnlyRuntime: SessionRuntimeInfo | null = session
    ? {
        sessionPk: session.sessionPk,
        model: agentModel,
        storedEffort: null,
        effectiveEffort: agentModelInfo?.resolvedDefault ?? null,
        effectiveEffortLabel:
          agentModelInfo?.supported.find((option) => option.value === agentModelInfo.resolvedDefault)?.label ??
          agentModelInfo?.resolvedDefault ??
          null,
        effectiveSource:
          agentModelInfo?.defaultSource === "configured"
            ? "configured"
            : agentModelInfo?.defaultSource === "provider"
              ? "provider"
              : "none",
        storedEffortStatus: "valid",
        modelInfo: agentModelInfo,
      }
    : null;
  const runtime =
    runtimeScope === "project"
      ? projectRuntime
      : runtimeScope === "session" && session
        ? (sessionRuntimeById[session.sessionPk] ?? null)
        : readOnlyRuntime;

  if (!session) {
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Select a session from the sidebar.</div>
    );
  }

  const meta = statusMeta(session.status);
  const running = session.status === "running";
  const pendingForSession = pendingApprovals.filter((a) => a.runnerId === runnerId && a.sessionPk === session.sessionPk);
  const permUi = corePermToUi(session.permMode);
  const permMeta = PERM_MODES.find((m) => m.id === permUi) ?? PERM_MODES[1];

  const submit = () => {
    const t = draft.trim();
    if (!t && composerFiles.attachments.length === 0) return;
    const key = session.sessionPk;
    const typed = draft;
    const options: ChatOptions = {
      context: { branch: session.branch, voiceTranscript: null, references: uniqueContextRefs(contextRefs) },
      attachments: composerFiles.attachments,
    };
    // Clear optimistically; a rejected *send* puts the text back. Enqueue never
    // fails, so no restore path there.
    useNav.getState().clearDraft(key);
    historyRef.current = HISTORY_IDLE;
    composerFiles.clear();
    setContextRefs([]);
    if (running) {
      enqueueMessage(runnerId, key, { id: crypto.randomUUID(), text: t, options });
      return;
    }
    void send(runnerId, key, t, options).then((ok) => {
      if (!ok) useNav.getState().restoreDraft(key, typed);
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
    <div className="relative flex min-h-0 flex-1 flex-col">
      {/* Workspace-level panel controls — always mounted at the workspace's
          top-right, independent of the chat header, so toggling either
          panel doesn't relocate or unmount these buttons. */}
      <div
        data-testid="session-panel-controls"
        className="absolute right-2.5 top-2.5 z-30 flex items-center gap-1 rounded-md border border-border bg-background/80 p-1 shadow-xs backdrop-blur"
      >
        {/* The disabled Button gets pointer-events-none, so its own `title`
            never fires a hover tooltip — a wrapping span (still hoverable)
            carries the "why disabled" tooltip. The Button keeps its normal
            title in both states so it still has a stable accessible name. */}
        <span title={isRemote ? "Not available for sessions on a remote runner" : undefined}>
          <Button
            variant="ghost"
            size="icon-sm"
            title="Toggle bottom panel"
            aria-pressed={nav.bottomOpen}
            onClick={nav.toggleBottom}
            disabled={isRemote}
            className={nav.bottomOpen ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
          >
            <PanelBottom aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
          </Button>
        </span>
        <Button
          variant="ghost"
          size="icon-sm"
          title="Toggle right panel"
          aria-pressed={nav.rightOpen}
          onClick={nav.toggleRight}
          className={nav.rightOpen ? "bg-accent text-accent-foreground" : "text-muted-foreground"}
        >
          <PanelRight aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
        </Button>
      </div>

      <div data-testid="session-main-row" className="flex min-h-0 min-w-0 flex-1">
        {/* Chat column */}
        <div className={`flex min-h-0 min-w-0 flex-1 flex-col ${nav.rightMaximized && nav.rightOpen ? "hidden" : ""}`}>
          <div
            data-testid="session-chat-header"
            className="box-border flex h-[55px] shrink-0 items-center gap-3 border-b border-border px-5 pr-[92px]"
          >
            <StatusDot color={meta.color} pulse={meta.pulse} size={9} />
            <div className="min-w-0">
              <div className="truncate text-sm font-semibold tracking-[-0.01em]">{session.title || "Untitled session"}</div>
              <div className="flex items-center gap-2.5 text-xs text-muted-foreground">
                <span>{headerAgentLine(project, agentModel)}</span>
                {session.branch && (
                  <span className="inline-flex items-center gap-1">
                    <GitBranch aria-hidden size={11} strokeWidth={2} />
                    {session.branch}
                  </span>
                )}
              </div>
            </div>
            <div className="flex-1" />
            <OpenInMenu runnerId={runnerId} sessionPk={session.sessionPk} />
          </div>

          {/* Transcript, with the floating plan panel overlaying it */}
          <div className="relative flex min-h-0 flex-1 flex-col">
            {/* Pinned orchestration task strip — only for a home chat with a live run */}
            {orchRootId && <TaskStrip rootId={orchRootId} />}
            <TranscriptFileContext.Provider value={transcriptFileCtx}>
              <Transcript
                runnerId={runnerId}
                sessionPk={session.sessionPk}
                rows={rows}
                agentName={NATIVE_AGENT.name}
                agentColor={NATIVE_AGENT.color}
                running={running}
              >
                {pendingForSession.map((a, i) => (
                  <div key={`${a.runnerId}:${a.runId}:${a.requestId}`} className="px-4 pb-2">
                    <ApprovalCard approval={a} hotkey={i === pendingForSession.length - 1} />
                  </div>
                ))}
              </Transcript>
            </TranscriptFileContext.Provider>
            {/* Agent plan (todowrite) — floating rounded panel */}
            <TodoPanel runnerId={runnerId} sessionPk={session.sessionPk} running={running} />
          </div>

          {/* Session composer */}
          <div className="shrink-0 px-6 pb-4 pt-3">
            <QueuedMessages runnerId={runnerId} sessionPk={session.sessionPk} />
            <div
              className={`acrylic-card relative mx-auto w-full max-w-3xl rounded-2xl border shadow-xs ${composerFiles.dragOver ? "border-primary" : "border-border"}`}
            >
              <Textarea
                value={draft}
                onChange={(e) => {
                  // Typing exits history mode: the edited text becomes the live draft.
                  historyRef.current = HISTORY_IDLE;
                  setDraft(e.target.value);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    submit();
                    return;
                  }
                  if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
                    const dir = e.key === "ArrowUp" ? ("up" as const) : ("down" as const);
                    const popupOpen = slashMatches.length > 0 || contextHits.length > 0;
                    const el = e.currentTarget;
                    if (!shouldNavigateHistory(dir, draft, el.selectionStart ?? 0, el.selectionEnd ?? 0, popupOpen)) return;
                    const step = stepHistory(dir, history, historyRef.current, draft);
                    if (!step) return;
                    e.preventDefault();
                    historyRef.current = step.state;
                    setDraft(step.text);
                  }
                }}
                onPaste={composerFiles.onPaste}
                placeholder={running ? "Enter to queue" : "Ask for follow-up changes"}
                className="max-h-[40vh] min-h-0 resize-none overflow-y-auto border-none bg-transparent px-4 pb-0.5 pt-[13px] text-[13.5px] leading-normal text-foreground focus-visible:ring-0 md:text-[13.5px] dark:bg-transparent"
              />
              {slashMatches.length > 0 && (
                <MenuPanel onClose={() => undefined} className="bottom-full left-2.5 z-50 mb-1.5 w-[320px]">
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
                <MenuPanel onClose={() => setContextHits([])} className="bottom-full left-2.5 z-50 mb-1.5 w-[360px]">
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
                    void setSessionPermMode(runnerId, session.sessionPk, uiPermToCore(mode as UiPermMode));
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
                <SessionCostPanel runnerId={runnerId} sessionPk={session.sessionPk} />
                <ComposerModelEffortMenu
                  models={agentModels}
                  runtime={runtime}
                  onChange={(model, effort) => {
                    if (runtimeScope === "project" && projectId) void setProjectRuntime(projectId, model, effort);
                    else if (runtimeScope === "session") void setSessionRuntime(runnerId, session.sessionPk, model, effort);
                  }}
                  disabled={agentModels.length === 0 || runtimeScope === null}
                  running={running}
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
                  <Button size="icon" title="Stop" onClick={() => void stop(runnerId, session.sessionPk)} className="rounded-full">
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
        </div>

        {/* Right panel — keyed by session so switching sessions remounts it: per-session
            review/file state resets, while diff data lives in the useDiff store keyed
            by sessionPk so sessions never see each other's results. */}
        {nav.rightOpen && (
          <RightPanel
            key={refKey({ runnerId, pk: session.sessionPk })}
            runnerId={runnerId}
            sessionPk={session.sessionPk}
            branch={session.branch ?? null}
            running={running}
            isGit={project?.isGit ?? false}
          />
        )}
      </div>

      {/* Bottom terminal drawer — a real shell in the session worktree, spanning
          the full workspace width below both the chat column and right panel.
          Gating on !isRemote here (not just disabling the toggle button) matters
          because nav.bottomOpen is a global, localStorage-persisted flag also
          toggled from TitleBar — without this render guard, switching into a
          remote session while the panel is already open would auto-spawn a
          PTY against a host that has none. */}
      {nav.bottomOpen && !isRemote && (
        <div data-testid="session-bottom-row" className="min-w-0 shrink-0">
          <BottomTerminalDrawer runnerId={runnerId} sessionPk={session.sessionPk} projectName={projectName} />
        </div>
      )}
    </div>
  );
}
