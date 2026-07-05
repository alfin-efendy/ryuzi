import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowUp, ChevronDown, CircleAlert, FileText, FolderOpen, GitBranch, Mic, Paperclip, Plus, X } from "lucide-react";
import { toast } from "sonner";
import {
  Button,
  MenuPanel,
  MenuPanelItem as MenuItem,
  MenuPanelSection as MenuSectionLabel,
  MenuPanelSeparator as MenuSeparator,
  Textarea,
} from "@ryuzi/ui";
import { commands } from "@/bindings";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { useNative } from "@/store-native";
import { HOME_SUGGESTIONS } from "@/constants";
import { chatRuntimeOf, useRuntimes } from "@/store-runtimes";
import { basename } from "@/lib/paths";
import { activeContextQuery, replaceActiveContextToken, uniqueContextRefs } from "@/lib/composer-context";
import { projectLabel } from "@/lib/sidebar";
import { AgentMenu } from "@/components/common/AgentMenu";
import { StatusDot } from "@/components/common/bits";
import { startVoiceDictation } from "@/lib/voice";

export function HomeView() {
  const { projects, sessions, selectedProjectId, selectProject, start, addProject } = useStore();
  const nav = useNav();
  const [draft, setDraft] = useState("");
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  const [projectMenuOpen, setProjectMenuOpen] = useState(false);
  const [branchMenuOpen, setBranchMenuOpen] = useState(false);
  const [attachments, setAttachments] = useState<string[]>([]);
  const [contextRefs, setContextRefs] = useState<string[]>([]);
  const [contextHits, setContextHits] = useState<string[]>([]);
  const [listening, setListening] = useState(false);
  const stopVoice = useRef<(() => void) | null>(null);

  const project = projects.find((p) => p.projectId === selectedProjectId) ?? projects[0];
  const runtimes = useRuntimes((s) => s.runtimes);
  const agent = chatRuntimeOf(runtimes, nav.composerAgent);
  const requestRuntimeId = agent?.id ?? (nav.composerAgent === "native" || nav.composerAgent === "claude" ? nav.composerAgent : null);
  const loadCommands = useNative((s) => s.loadCommands);
  const nativeCommands = useNative((s) => (project ? (s.commandsByProject[project.projectId] ?? []) : []));

  useEffect(() => {
    if (project && requestRuntimeId === "native") void loadCommands(project.projectId);
  }, [project?.projectId, requestRuntimeId, loadCommands]);

  const branches = useMemo(() => {
    const fromSessions = sessions.filter((s) => s.projectId === project?.projectId && s.branch).map((s) => s.branch as string);
    return [...new Set(["main", ...fromSessions])];
  }, [sessions, project?.projectId]);

  const slashQuery = useMemo(() => {
    const trimmed = draft.trimStart();
    if (!trimmed.startsWith("/") || trimmed.includes(" ")) return null;
    return trimmed.slice(1).toLowerCase();
  }, [draft]);
  const slashMatches = useMemo(() => {
    if (requestRuntimeId !== "native" || slashQuery === null) return [];
    return nativeCommands.filter((c) => c.name.toLowerCase().startsWith(slashQuery)).slice(0, 6);
  }, [requestRuntimeId, nativeCommands, slashQuery]);
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

  const send = async () => {
    const t = draft.trim();
    if ((!t && attachments.length === 0) || !project) return;
    const opts = {
      runtimeId: requestRuntimeId,
      model: agent?.model || null,
      context: { branch: nav.composerBranch, voiceTranscript: null, references: uniqueContextRefs(contextRefs) },
      attachments,
    };
    setDraft("");
    setAttachments([]);
    setContextRefs([]);
    await start(project.projectId, t, opts);
    nav.navigate({ kind: "session" });
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-7 p-10">
      <h1 className="m-0 text-center text-[30px] font-semibold tracking-[-0.02em]">
        What should we build{project ? ` in ${projectLabel(project)}` : ""}?
      </h1>
      <div className="w-full max-w-[720px]">
        <div className="acrylic-card relative rounded-2xl border border-border shadow-sm">
          <Textarea
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
            className="field-sizing-fixed min-h-0 resize-none border-none bg-transparent px-[18px] pb-1 pt-4 text-[14.5px] leading-normal text-foreground focus-visible:ring-0 md:text-[14.5px] dark:bg-transparent"
          />
          {slashMatches.length > 0 && (
            <MenuPanel onClose={() => undefined} className="bottom-[86px] left-3 z-50 w-[320px]">
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
            <MenuPanel onClose={() => setContextHits([])} className="bottom-[86px] left-3 z-50 w-[360px]">
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
            <Button variant="ghost" size="icon-sm" title="Attach" onClick={() => void attachFiles()} className="rounded-full text-muted-foreground">
              <Paperclip aria-hidden size={16} strokeWidth={2} />
            </Button>
            <Button variant="ghost" className="font-medium" style={{ color: "#E8703A" }}>
              <CircleAlert aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              Full access
              <ChevronDown aria-hidden size={12} strokeWidth={2} className="size-3" />
            </Button>
            <div className="flex-1" />
            <Button variant="ghost" onClick={() => setAgentMenuOpen((v) => !v)} className="font-semibold">
              <StatusDot color={agent?.color ?? "var(--muted-foreground)"} />
              {agent?.model || agent?.name || "No agent"}
              <span className="font-normal text-muted-foreground">{agent?.name ?? "install one"}</span>
              <ChevronDown aria-hidden size={12} strokeWidth={2} className="size-3" />
            </Button>
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

            {agentMenuOpen && (
              <AgentMenu value={agent?.id ?? nav.composerAgent} onPick={nav.setComposerAgent} onClose={() => setAgentMenuOpen(false)} />
            )}
          </div>
          {(attachments.length > 0 || contextRefs.length > 0) && (
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

          {/* Context chips */}
          <div className="relative flex items-center gap-1.5 border-t border-border px-3 py-2">
            <Button variant="ghost" size="sm" onClick={() => setProjectMenuOpen((v) => !v)} className="gap-[7px] font-semibold">
              <FolderOpen aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              {project ? projectLabel(project) : "No project"}
              <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setBranchMenuOpen((v) => !v)}
              className="gap-[7px] font-medium text-muted-foreground"
            >
              <GitBranch aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              {nav.composerBranch}
              <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
            </Button>

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
            <Button key={s} variant="outline" onClick={() => setDraft(s)} className="rounded-full px-3 text-muted-foreground">
              {s}
            </Button>
          ))}
        </div>
      </div>
    </div>
  );
}
