import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowUp, FileText, GitBranch, Mic, PanelBottom, PanelRight, Paperclip, X } from "lucide-react";
import { toast } from "sonner";
import { Button, MenuPanel, MenuPanelItem as MenuItem, MenuPanelSection as MenuSectionLabel, Textarea } from "@ryuzi/ui";
import { commands, type AgentSummaryInfo, type TurnInput } from "@/bindings";
import { useStore, type ChatOptions } from "@/store";
import { LOCAL_RUNNER, isSession, refKey } from "@/lib/session-key";
import { useNav } from "@/store-nav";
import { useDiff } from "@/store-diff";
import { useNative } from "@/store-native";
import { useAgents } from "@/store-agents";
import { delegationSessionKey, useDelegation } from "@/store-delegation";
import { statusMeta } from "@/lib/status";
import { projectLabel } from "@/lib/sidebar";
import { sessionIsReadOnly, sessionPrimaryLabel } from "@/lib/session-primary";
import { activeAgentMentionQuery, insertAgentMention, matchMentionAgents, updateMentionDraft, type MentionDraft } from "@/lib/mentions";
import { AgentMentionMenu } from "@/components/composer/AgentMentionMenu";
import { activeContextQuery, replaceActiveContextToken, uniqueContextRefs } from "@/lib/composer-context";
import { ApprovalCard } from "@/components/approval/ApprovalCard";
import { StatusDot } from "@/components/common/bits";
import { Transcript } from "@/components/transcript/Transcript";
import { TranscriptFileContext } from "@/components/transcript/TranscriptFileContext";
import { RightPanel } from "@/components/session/RightPanel";
import { BottomTerminalDrawer } from "@/components/session/BottomTerminalDrawer";
import { TodoPanel } from "@/components/session/TodoPanel";
import { OpenInMenu } from "@/components/session/OpenInMenu";
import { QueuedMessages } from "@/components/session/QueuedMessages";
import { startVoiceDictation } from "@/lib/voice";
import { useComposerAttachments } from "@/components/composer/useComposerAttachments";
import { AttachmentChips } from "@/components/composer/AttachmentChips";
import { HISTORY_IDLE, historyEntries, shouldNavigateHistory, stepHistory, type HistoryState } from "@/components/composer/inputHistory";

export function SessionView() {
  const { sessions, transcripts, focusedSession, send, stop, pendingApprovals, projects } = useStore();
  const enqueueQueueMessage = useNative((s) => s.enqueueQueueMessage);
  const nav = useNav();
  // Draft text lives in the persisted useNav drafts map keyed by session, so
  // switching sessions/views (SessionView renders un-keyed in App.tsx) swaps
  // the visible text instead of leaking one session's draft into another.
  const draftKey = focusedSession ? refKey(focusedSession) : "";
  const draft = nav.drafts[draftKey] ?? "";
  const session = sessions.find((s) => isSession(s, focusedSession));
  const runnerId = session?.runnerId ?? LOCAL_RUNNER;
  const mountedSessionPk = session?.sessionPk ?? null;
  const delegationKey = mountedSessionPk ? delegationSessionKey(runnerId, mountedSessionPk) : null;
  const rootRunId = useDelegation((state) => (delegationKey ? (state.rootRunBySession[delegationKey] ?? null) : null));
  const loadDelegation = useDelegation((state) => state.load);
  // A local ConPTY/bash and locally-installed apps can't operate on a remote
  // host's workdir — the bottom terminal drawer and Open-in menu are gated
  // off entirely for sessions running on a non-local runner.
  const isRemote = runnerId !== LOCAL_RUNNER;
  const composerFiles = useComposerAttachments(runnerId);
  const [contextRefs, setContextRefs] = useState<string[]>([]);
  const [mentionsByDraft, setMentionsByDraft] = useState<Record<string, MentionDraft["mentions"]>>({});
  const mentionsByDraftRef = useRef<Record<string, MentionDraft["mentions"]>>({});
  mentionsByDraftRef.current = mentionsByDraft;
  const mentions = mentionsByDraft[draftKey] ?? [];
  const setMentions = useCallback(
    (next: MentionDraft["mentions"] | ((current: MentionDraft["mentions"]) => MentionDraft["mentions"])) => {
      const currentMentions = mentionsByDraftRef.current[draftKey] ?? [];
      const nextMentions = typeof next === "function" ? next(currentMentions) : next;
      mentionsByDraftRef.current = { ...mentionsByDraftRef.current, [draftKey]: nextMentions };
      setMentionsByDraft((current) => ({ ...current, [draftKey]: nextMentions }));
    },
    [draftKey],
  );
  const [mentionCaret, setMentionCaret] = useState(0);
  const updateDraft = useCallback(
    (next: string | ((current: string) => string) | MentionDraft) => {
      const { drafts, setDraft: write } = useNav.getState();
      const current = drafts[draftKey] ?? "";
      const currentMentions = mentionsByDraftRef.current[draftKey] ?? [];
      const updated =
        typeof next === "object"
          ? next
          : updateMentionDraft({ text: current, mentions: currentMentions }, typeof next === "function" ? next(current) : next);
      write(draftKey, updated.text);
      setMentions(updated.mentions);
    },
    [draftKey, setMentions],
  );
  const [mentionActiveIndex, setMentionActiveIndex] = useState(0);
  const [contextHits, setContextHits] = useState<string[]>([]);
  const [listening, setListening] = useState(false);
  const submitInFlight = useRef(false);
  const [submitting, setSubmitting] = useState(false);
  const stopVoice = useRef<(() => void) | null>(null);

  const rows = (focusedSession && transcripts[refKey(focusedSession)]) || [];
  const registry = useAgents((s) => s.registry);
  const project = projects.find((p) => p.projectId === session?.projectId);
  const projectId = project?.projectId;
  const projectName = project ? projectLabel(project) : (session?.projectId ?? "");
  const loadCommands = useNative((s) => s.loadCommands);
  const nativeCommands = useNative((s) => (project ? (s.commandsByProject[project.projectId] ?? []) : []));

  // Hydrate child-run metadata as soon as this transcript is mounted. The
  // store deduplicates requests, so opening the Agents panel never creates a
  // second request for the same runner/session identity.
  useEffect(() => {
    if (mountedSessionPk) void loadDelegation(runnerId, mountedSessionPk);
  }, [loadDelegation, runnerId, mountedSessionPk]);

  useEffect(() => {
    // Slash commands are project metadata on the local engine.
    if (projectId) void loadCommands(LOCAL_RUNNER, projectId);
  }, [projectId, loadCommands]);

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
        updateDraft(historyRef.current.pending);
      }
    };
  }, [updateDraft]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reset transient composer state when the focused session changes
  useEffect(() => {
    setMentionCaret(0);
    setMentionActiveIndex(0);
    setContextRefs([]);
    setContextHits([]);
  }, [draftKey]);

  const slashQuery = useMemo(() => {
    const trimmed = draft.trimStart();
    if (!trimmed.startsWith("/") || trimmed.includes(" ")) return null;
    return trimmed.slice(1).toLowerCase();
  }, [draft]);
  const slashMatches = useMemo(() => {
    if (slashQuery === null) return [];
    return nativeCommands
      .filter((c) => c.effective)
      .filter((c) => c.name.toLowerCase().startsWith(slashQuery))
      .slice(0, 6);
  }, [nativeCommands, slashQuery]);
  const mentionQuery = useMemo(() => activeAgentMentionQuery(draft, mentionCaret), [draft, mentionCaret]);
  const mentionMatches = useMemo(
    () => matchMentionAgents(registry?.agents ?? [], mentionQuery?.query ?? "", session?.primaryAgentId ?? null, mentions),
    [registry?.agents, mentionQuery?.query, session?.primaryAgentId, mentions],
  );
  const contextQuery = useMemo(() => activeContextQuery(draft), [draft]);
  const contextQueryText = contextQuery?.query ?? null;
  const mentionMenuOpen = mentionQuery !== null && contextQuery === null && slashQuery === null && mentionMatches.length > 0;

  useEffect(() => {
    if (!projectId || contextQueryText === null) {
      setContextHits([]);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      void commands.searchFiles(LOCAL_RUNNER, projectId, contextQueryText).then((res) => {
        if (!cancelled) {
          setContextHits(
            res.status === "ok"
              ? res.data
                  .filter((entry) => !entry.dir)
                  .map((entry) => entry.path)
                  .slice(0, 6)
              : [],
          );
        }
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
  const pendingForSession = pendingApprovals.filter((a) => a.runnerId === runnerId && a.sessionPk === session.sessionPk);
  const currentPrimary = session.primaryAgentId ? (registry?.agents.find((agent) => agent.id === session.primaryAgentId) ?? null) : null;
  const composeReadOnly = sessionIsReadOnly(session.primaryAgentSnapshot) || currentPrimary === null || !currentPrimary.executable;
  const composeReadOnlyReason = sessionIsReadOnly(session.primaryAgentSnapshot)
    ? "Legacy sessions are read-only."
    : currentPrimary === null
      ? "The session’s primary agent was deleted, so this session is read-only."
      : "The session’s primary agent is not executable.";

  const submit = () => {
    if (composeReadOnly || submitInFlight.current) return;
    const t = draft;
    if (!t.trim() && composerFiles.attachments.length === 0) return;
    const key = session.sessionPk;
    const typed = draft;
    const typedMentions = mentions;
    const options: ChatOptions = {
      mentions,
      context: { branch: session.branch, voiceTranscript: null, references: uniqueContextRefs(contextRefs) },
      attachments: composerFiles.attachments,
    };
    const turn: TurnInput = {
      text: t,
      mentions,
      context: {
        branch: options.context?.branch ?? null,
        voiceTranscript: options.context?.voiceTranscript ?? null,
        references: options.context?.references ?? [],
      },
      attachments: options.attachments ?? [],
      git: null,
    };
    if (running) {
      // The durable queue backend accepts ChatRequestOptions, which currently
      // has no structured mentions field. Preserve the full TurnInput for
      // immediate sends; queued prompts cross the generated IPC boundary with
      // only its supported options.
      const queueOptions = {
        model: null,
        effort: null,
        context: turn.context,
        attachments: turn.attachments,
        git: turn.git,
        permMode: null,
      };
      submitInFlight.current = true;
      setSubmitting(true);
      void enqueueQueueMessage(runnerId, key, t, queueOptions)
        .then((ok) => {
          if (ok) {
            useNav.getState().clearDraft(draftKey);
            historyRef.current = HISTORY_IDLE;
            composerFiles.clear();
            setContextRefs([]);
            setMentions([]);
          } else {
            useNav.getState().restoreDraft(draftKey, typed);
            setMentions(typedMentions);
          }
        })
        .catch(() => {
          useNav.getState().restoreDraft(draftKey, typed);
          setMentions(typedMentions);
        })
        .finally(() => {
          submitInFlight.current = false;
          setSubmitting(false);
        });
      return;
    }
    submitInFlight.current = true;
    setSubmitting(true);
    void send(runnerId, key, turn)
      .then((ok) => {
        if (ok) {
          useNav.getState().clearDraft(draftKey);
          historyRef.current = HISTORY_IDLE;
          composerFiles.clear();
          setContextRefs([]);
          setMentions([]);
        } else {
          useNav.getState().restoreDraft(draftKey, typed);
          setMentions(typedMentions);
        }
      })
      .catch(() => {
        useNav.getState().restoreDraft(draftKey, typed);
        setMentions(typedMentions);
      })
      .finally(() => {
        submitInFlight.current = false;
        setSubmitting(false);
      });
  };

  const pickContext = (path: string) => {
    updateDraft((cur) => replaceActiveContextToken(cur, path));
    setContextRefs((cur) => uniqueContextRefs([...cur, path]));
    setContextHits([]);
  };

  const pickMention = (agent: AgentSummaryInfo) => {
    const next = insertAgentMention({ text: draft, mentions }, mentionCaret, agent);
    updateDraft(next);
    setMentionCaret(next.text.length);
    setMentionActiveIndex(0);
  };

  const dismissMentionMenu = () => {
    setMentionCaret(0);
    setMentionActiveIndex(0);
  };

  const toggleVoice = () => {
    if (listening) {
      stopVoice.current?.();
      stopVoice.current = null;
      setListening(false);
      return;
    }
    const started = startVoiceDictation({
      onText: (text) => updateDraft((cur) => (cur ? `${cur} ${text}` : text)),
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
                <span>{sessionPrimaryLabel(session.primaryAgentSnapshot, registry?.agents)}</span>
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

          {/* Transcript, with the TODO List overlaying it */}
          <div className="relative flex min-h-0 flex-1 flex-col">
            <TodoPanel runnerId={runnerId} sessionPk={session.sessionPk} running={running} />
            <TranscriptFileContext.Provider value={transcriptFileCtx}>
              <Transcript
                runnerId={runnerId}
                sessionPk={session.sessionPk}
                rows={rows}
                agentName={sessionPrimaryLabel(session.primaryAgentSnapshot, registry?.agents)}
                agentColor={session.primaryAgentSnapshot?.avatarColor ?? "#71717A"}
                running={running}
                ownerRunId={rootRunId}
              >
                {pendingForSession.map((a, i) => (
                  <div key={`${a.runnerId}:${a.runId}:${a.requestId}`} className="px-4 pb-2">
                    <ApprovalCard approval={a} hotkey={i === pendingForSession.length - 1} />
                  </div>
                ))}
              </Transcript>
            </TranscriptFileContext.Provider>
          </div>

          {/* Session composer */}
          <div className="shrink-0 px-6 pb-4 pt-3">
            <QueuedMessages runnerId={runnerId} sessionPk={session.sessionPk} />
            {composeReadOnly && (
              <div className="mx-auto flex w-full max-w-3xl items-center justify-between gap-3 px-3 pb-2 text-xs text-muted-foreground">
                <span>{composeReadOnlyReason}</span>
                {currentPrimary && !currentPrimary.executable ? (
                  <Button variant="outline" size="sm" onClick={() => nav.navigate({ kind: "agentDetail", agentId: currentPrimary.id })}>
                    Repair agent
                  </Button>
                ) : null}
              </div>
            )}
            <div
              className={`acrylic-card relative mx-auto w-full max-w-3xl rounded-2xl border shadow-xs ${composerFiles.dragOver ? "border-primary" : "border-border"}`}
            >
              <Textarea
                value={draft}
                onChange={(e) => {
                  // Typing exits history mode: the edited text becomes the live draft.
                  historyRef.current = HISTORY_IDLE;
                  updateDraft(e.target.value);
                  setMentionCaret(e.target.selectionStart);
                  setMentionActiveIndex(0);
                }}
                onSelect={(e) => setMentionCaret(e.currentTarget.selectionStart)}
                onKeyDown={(e) => {
                  if (e.key === "Escape" && mentionMenuOpen) {
                    e.preventDefault();
                    dismissMentionMenu();
                    return;
                  }
                  if (mentionMenuOpen && (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === "Tab")) {
                    const delta = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
                    e.preventDefault();
                    if (delta) setMentionActiveIndex((index) => (index + delta + mentionMatches.length) % mentionMatches.length);
                    else {
                      const agent = mentionMatches[mentionActiveIndex];
                      if (agent) pickMention(agent);
                    }
                    return;
                  }
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    void submit();
                    return;
                  }
                  if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
                    const dir = e.key === "ArrowUp" ? ("up" as const) : ("down" as const);
                    const popupOpen = slashMatches.length > 0 || contextHits.length > 0 || mentionMenuOpen;
                    const el = e.currentTarget;
                    if (!shouldNavigateHistory(dir, draft, el.selectionStart ?? 0, el.selectionEnd ?? 0, popupOpen)) return;
                    const step = stepHistory(dir, history, historyRef.current, draft);
                    if (!step) return;
                    e.preventDefault();
                    historyRef.current = step.state;
                    updateDraft(step.text);
                  }
                }}
                onPaste={composerFiles.onPaste}
                disabled={composeReadOnly}
                placeholder={composeReadOnly ? composeReadOnlyReason : running ? "Enter to queue" : "Ask for follow-up changes"}
                className="max-h-[40vh] min-h-0 resize-none overflow-y-auto border-none bg-transparent px-4 pb-0.5 pt-[13px] text-[13.5px] leading-normal text-foreground focus-visible:ring-0 md:text-[13.5px] dark:bg-transparent"
              />
              {mentionMenuOpen && (
                <AgentMentionMenu
                  agents={mentionMatches}
                  activeIndex={mentionActiveIndex}
                  onActiveIndexChange={setMentionActiveIndex}
                  onPick={pickMention}
                  onClose={dismissMentionMenu}
                />
              )}
              {slashMatches.length > 0 && (
                <MenuPanel onClose={() => undefined} className="bottom-full left-2.5 z-50 mb-1.5 w-[320px]">
                  <MenuSectionLabel>Commands</MenuSectionLabel>
                  {slashMatches.map((cmd) => (
                    <MenuItem key={cmd.name} onClick={() => updateDraft(`/${cmd.name} `)} className="font-medium">
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
                  disabled={composeReadOnly}
                  className="rounded-full text-muted-foreground"
                >
                  <Paperclip aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
                </Button>
                <div className="flex-1" />
                <Button
                  variant="ghost"
                  size="icon-sm"
                  title="Voice"
                  onClick={toggleVoice}
                  disabled={composeReadOnly}
                  className={`rounded-full ${listening ? "bg-accent text-accent-foreground" : "text-muted-foreground"}`}
                >
                  <Mic aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
                </Button>
                {running ? (
                  <Button size="icon" title="Stop" onClick={() => void stop(runnerId, session.sessionPk)} className="rounded-full">
                    <span className="h-[11px] w-[11px] rounded-[2px] bg-current" />
                  </Button>
                ) : (
                  <Button size="icon" title="Send" onClick={submit} disabled={composeReadOnly || submitting} className="rounded-full">
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
