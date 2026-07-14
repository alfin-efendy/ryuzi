import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowUp, ChevronDown, FileText, FolderOpen, GitBranch, Mic, Paperclip, Plus, X } from "lucide-react";
import { toast } from "sonner";
import { Button, Combobox, MenuPanel, MenuPanelItem as MenuItem, MenuPanelSection as MenuSectionLabel, Switch, Textarea } from "@ryuzi/ui";
import { commands, type BranchList } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";
import { useStore } from "@/store";
import { useNav, choosePrimaryAgent, LAST_PRIMARY_AGENT_KEY } from "@/store-nav";
import { useNative } from "@/store-native";
import { useConnections } from "@/store-connections";
import { HOME_SUGGESTIONS } from "@/constants";
import { useAgents } from "@/store-agents";
import { activeContextQuery, replaceActiveContextToken, uniqueContextRefs } from "@/lib/composer-context";
import { composerGitOptionsForProject, normalizeBranchName } from "@/lib/composer-git";
import { projectLabel } from "@/lib/sidebar";
import { startVoiceDictation } from "@/lib/voice";
import { useComposerAttachments } from "@/components/composer/useComposerAttachments";
import { AttachmentChips } from "@/components/composer/AttachmentChips";
import { AddProjectModal } from "@/components/modals/AddProjectModal";
import { BranchNameModal } from "@/components/modals/BranchNameModal";

// Sentinel Combobox value for "no project attached" — Base UI's Combobox
// value type is a plain string, so a real project id can't share the slot
// with null. Detaching (picking this) clears selectProject back to null.
const NO_PROJECT = "__none__";

export function HomeView() {
  const {
    projects,
    selectedProjectId,
    selectProject,
    start,
    startChat,
  } = useStore();
  const nav = useNav();
  const [addProjectOpen, setAddProjectOpen] = useState(false);
  const composerFiles = useComposerAttachments();
  const [contextRefs, setContextRefs] = useState<string[]>([]);
  const [contextHits, setContextHits] = useState<string[]>([]);
  const [listening, setListening] = useState(false);
  const stopVoice = useRef<(() => void) | null>(null);

  // Chat is the default: no project is auto-selected. A project is attached
  // only when the user explicitly picks one (sidebar "+" or the composer's
  // project Combobox) — see selectedProjectId in the store.
  const project = projects.find((p) => p.projectId === selectedProjectId);
  const projectId = project?.projectId;
  const draftKey = `home:${projectId ?? ""}`;
  const draft = nav.drafts[draftKey] ?? "";
  const setDraft = useCallback(
    (next: string | ((cur: string) => string)) => {
      const { drafts, setDraft: write } = useNav.getState();
      write(draftKey, typeof next === "function" ? next(drafts[draftKey] ?? "") : next);
    },
    [draftKey],
  );
  const isGit = project?.isGit ?? false;
  const registry = useAgents((s) => s.registry);
  const loadCommands = useNative((s) => s.loadCommands);
  const nativeCommands = useNative((s) => (project ? (s.commandsByProject[project.projectId] ?? []) : []));
  const connectionsLoaded = useConnections((s) => s.loaded);
  const hydrateConnections = useConnections((s) => s.hydrate);

  useEffect(() => {
    // Slash commands are project metadata on the local engine.
    if (projectId) void loadCommands(LOCAL_RUNNER, projectId);
  }, [projectId, loadCommands]);

  const [branchList, setBranchList] = useState<BranchList | null>(null);
  const [branchModalOpen, setBranchModalOpen] = useState(false);
  const setComposerBranch = nav.setComposerBranch;

  useEffect(() => {
    setBranchList(null);
    setComposerBranch(null);
    // Non-git projects have no branches — never call list_branches for them
    // (it errors "not a git repository").
    if (!projectId || !isGit) return;
    let cancelled = false;
    void commands.listBranches(LOCAL_RUNNER, projectId).then((res) => {
      if (cancelled) return;
      if (res.status === "ok") {
        setBranchList(res.data);
        // A detached HEAD's "current" is a short commit id, not a branch name —
        // preselecting it would let the user unknowingly create a branch named
        // after a commit id. Leave the selection null; the placeholder shows.
        if (!res.data.detached) setComposerBranch(res.data.current);
      } else {
        toast.error("Couldn't list branches: " + res.error.message);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [projectId, isGit, setComposerBranch]);

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

  const send = async () => {
    const text = draft.trim();
    if (!text && composerFiles.attachments.length === 0) return;
    const primaryAgentId = choosePrimaryAgent(
      registry?.agents ?? [],
      useNav.getState().consumePendingPrimaryAgentId(),
      localStorage.getItem(LAST_PRIMARY_AGENT_KEY),
      registry?.defaultAgentId ?? null,
    );
    if (!primaryAgentId) {
      toast.error("No executable agent is available.");
      return;
    }
    const typed = draft;
    const turn = {
      text,
      context: { branch: isGit ? nav.composerBranch : null, voiceTranscript: null, references: uniqueContextRefs(contextRefs) },
      attachments: composerFiles.attachments,
      git: composerGitOptionsForProject(isGit, branchList, nav.composerBranch, nav.composerUseWorktree),
    };
    useNav.getState().clearDraft(draftKey);
    composerFiles.clear();
    setContextRefs([]);
    const ok = project
      ? await start(LOCAL_RUNNER, project.projectId, primaryAgentId, turn)
      : await startChat(LOCAL_RUNNER, primaryAgentId, turn);
    if (ok) {
      localStorage.setItem(LAST_PRIMARY_AGENT_KEY, primaryAgentId);
      nav.navigate({ kind: "session" });
    } else useNav.getState().restoreDraft(draftKey, typed);
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-7 p-10">
      <h1 className="m-0 text-center text-[30px] font-semibold tracking-[-0.02em]">
        What should we build{project ? ` in ${projectLabel(project)}` : ""}?
      </h1>
      <div className="w-full max-w-[720px]">
        <div
          className={`acrylic-card relative rounded-2xl border shadow-sm ${composerFiles.dragOver ? "border-primary" : "border-border"}`}
        >
          <Textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            onPaste={composerFiles.onPaste}
            placeholder="Do anything"
            className="max-h-[40vh] resize-none overflow-y-auto border-none bg-transparent px-[18px] pb-1 pt-4 text-[14.5px] leading-normal text-foreground focus-visible:ring-0 md:text-[14.5px] dark:bg-transparent"
          />
          {slashMatches.length > 0 && (
            <MenuPanel onClose={() => undefined} className="bottom-full left-3 z-50 mb-1.5 w-[320px]">
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
            <MenuPanel onClose={() => setContextHits([])} className="bottom-full left-3 z-50 mb-1.5 w-[360px]">
              <MenuSectionLabel>Context</MenuSectionLabel>
              {contextHits.map((path) => (
                <MenuItem key={path} onClick={() => pickContext(path)} className="font-medium">
                  <FileText aria-hidden size={13} strokeWidth={2} className="size-[13px] text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{path}</span>
                </MenuItem>
              ))}
            </MenuPanel>
          )}
          <div className="relative flex items-center gap-1.5 px-3 pb-3 pt-2">
            <Button
              variant="ghost"
              size="icon-sm"
              title="Attach"
              onClick={() => void composerFiles.attachFiles()}
              className="rounded-full text-muted-foreground"
            >
              <Paperclip aria-hidden size={16} strokeWidth={2} />
            </Button>
            <div className="flex-1" />
            <Button
              variant="ghost"
              size="icon-sm"
              title="Voice"
              onClick={toggleVoice}
              className={`rounded-full ${listening ? "bg-accent text-accent-foreground" : "text-muted-foreground"}`}
            >
              <Mic aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            </Button>
            <Button size="icon" title="Start session" onClick={() => void send()} className="rounded-full">
              <ArrowUp aria-hidden size={15} strokeWidth={2.2} className="size-[15px]" />
            </Button>
          </div>
          {(composerFiles.attachments.length > 0 || contextRefs.length > 0) && (
            <div className="flex flex-wrap gap-1.5 px-3 pb-2">
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

          {/* Context chips */}
          <div className="relative flex items-center gap-1.5 border-t border-border px-3 py-2">
            <Combobox
              aria-label="Project"
              options={[
                { value: NO_PROJECT, label: "No project" },
                ...projects.map((p) => ({ value: p.projectId, label: projectLabel(p) })),
              ]}
              value={project?.projectId ?? NO_PROJECT}
              onValueChange={(id) => selectProject(id === NO_PROJECT ? null : id)}
              placeholder="No project"
              trigger={
                <Button variant="ghost" size="sm" className="gap-[7px] font-semibold">
                  <FolderOpen aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
                  {project ? projectLabel(project) : "No project"}
                  <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                </Button>
              }
              footer={
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setAddProjectOpen(true)}
                  className="w-full justify-start gap-2 font-medium text-muted-foreground hover:text-accent-foreground"
                >
                  <Plus aria-hidden size={13} strokeWidth={2} />
                  New project
                </Button>
              }
            />
            {isGit && (
              <Combobox
                aria-label="Branch"
                popupClassName="w-64 max-w-[var(--available-width)]"
                options={(branchList?.branches ?? []).map((b) => ({ value: b, label: b, mono: true }))}
                value={nav.composerBranch}
                onValueChange={(v) => nav.setComposerBranch(v)}
                allowCreate
                onCreate={(input) => nav.setComposerBranch(normalizeBranchName(input))}
                createHintLabel="New Branch"
                onCreateHint={() => setBranchModalOpen(true)}
                placeholder="Branch"
                trigger={
                  <Button variant="ghost" size="sm" className="gap-[7px] font-medium text-muted-foreground">
                    <GitBranch aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
                    {nav.composerBranch ?? "branch"}
                    <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                  </Button>
                }
                footer={
                  <div className="flex items-center justify-between gap-3 px-2.5 py-1.5">
                    <span className="text-sm text-muted-foreground" title="Run the session in an isolated git worktree">
                      Worktree
                    </span>
                    <Switch
                      on={nav.composerUseWorktree}
                      onToggle={() => nav.setComposerUseWorktree(!nav.composerUseWorktree)}
                      label="Worktree"
                    />
                  </div>
                }
              />
            )}
          </div>
        </div>

        <div className="mt-4 flex flex-wrap justify-center gap-2">
          {HOME_SUGGESTIONS.map((s) => (
            <Button key={s} variant="outline" onClick={() => setDraft(s)} className="rounded-full px-3 text-muted-foreground">
              {s}
            </Button>
          ))}
        </div>
        <BranchNameModal
          open={branchModalOpen}
          onClose={() => setBranchModalOpen(false)}
          existingBranches={branchList?.branches ?? []}
          onCreate={(name) => nav.setComposerBranch(name)}
        />
      </div>
      <AddProjectModal open={addProjectOpen} onClose={() => setAddProjectOpen(false)} />
    </div>
  );
}
