// apps/cockpit/src/components/shell/Sidebar.tsx
import { useEffect, useRef, useState } from "react";
import {
  Archive,
  Bot,
  Workflow,
  ChevronRight,
  ChevronsUpDown,
  Folder,
  FolderOpen,
  FolderPlus,
  Grip,
  Inbox,
  LayoutGrid,
  ListFilter,
  Pencil,
  Pin,
  Plus,
  Server,
  Settings,
} from "lucide-react";
import {
  Badge,
  Button,
  MenuPanel,
  MenuPanelItem as MenuItem,
  MenuPanelSection as MenuSectionLabel,
  MenuPanelSeparator as MenuSeparator,
  Modal,
  ModalBody,
  ModalFooter,
  ModalHeader,
} from "@ryuzi/ui";
import { DndContext, closestCenter, KeyboardSensor, PointerSensor, useSensor, useSensors, type DragEndEvent } from "@dnd-kit/core";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { SortableContext, sortableKeyboardCoordinates, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { useStore } from "@/store";
import { useUi } from "@/store-ui";
import { useNav, type View } from "@/store-nav";
import { useGateways } from "@/store-gateways";
import { useTerms } from "@/store-terms";
import { commands } from "@/bindings";
import {
  archivedCount,
  chatSessions,
  isUnreadVisible,
  orderProjects,
  projectLabel,
  sessionTitle,
  sessionsForProject,
  type Ordering,
} from "@/lib/sidebar";
import { LOCAL_RUNNER, isSession, refOf, sessionKey, type UiSession } from "@/lib/session-key";
import { statusMeta } from "@/lib/status";
import { StatusDot, TreeGuide } from "@/components/common/bits";
import { AddProjectModal } from "@/components/modals/AddProjectModal";
import { SessionRow } from "@/components/shell/SessionRow";
import { SortableSessionRow } from "@/components/shell/SortableSessionRow";

const NAV: { label: string; icon: typeof Pencil; view: View; group: View["kind"][] }[] = [
  { label: "New Task", icon: Pencil, view: { kind: "home" }, group: ["home"] },
  { label: "Inbox", icon: Inbox, view: { kind: "inbox" }, group: ["inbox"] },
  { label: "Models", icon: Grip, view: { kind: "models" }, group: ["models", "providerDetail"] },
  { label: "Automations", icon: Workflow, view: { kind: "automations" }, group: ["automations", "scheduler", "jobDetail", "jobNew"] },
  { label: "Plugins", icon: LayoutGrid, view: { kind: "plugins" }, group: ["plugins", "appDetail", "pluginDetail"] },
  { label: "Agents", icon: Bot, view: { kind: "agents" }, group: ["agents", "agentDetail"] },
  { label: "Settings", icon: Settings, view: { kind: "settings" }, group: ["settings"] },
];

// Layout-only overrides for the tiny ghost icon Buttons in tree rows.
const iconBtn = "shrink-0 rounded-sm text-muted-foreground";

// Slim Organize/Ordering popover shared by the Tasks and Projects headers —
// each caller supplies its own open/close state and the Ordering slice it
// controls (task vs. project), but "Organize" (chat-first vs. per-project
// grouping) is global, so both instances read/write the same organizeBy.
function OrganizeMenu({
  open,
  onClose,
  organizeBy,
  setOrganizeBy,
  ordering,
  setOrdering,
  className,
}: {
  open: boolean;
  onClose: () => void;
  organizeBy: "project" | "task";
  setOrganizeBy: (v: "project" | "task") => void;
  ordering: Ordering;
  setOrdering: (o: Ordering) => void;
  className: string;
}) {
  const [orgSub, setOrgSub] = useState(false);
  const [ordSub, setOrdSub] = useState(false);
  if (!open) return null;
  const orgLabel = organizeBy === "task" ? "By Task" : "By Project";
  const ordLabel = ordering === "name" ? "Name" : ordering === "manual" ? "Manual Order" : "Updated";
  return (
    <MenuPanel onClose={onClose} className={className}>
      <MenuItem
        onClick={() => {
          setOrgSub((v) => !v);
          setOrdSub(false);
        }}
      >
        <span className="flex-1">Organize</span>
        <span className="flex items-center gap-1 text-[12.5px] text-muted-foreground">
          {orgLabel} <ChevronRight aria-hidden size={11} strokeWidth={2} />
        </span>
      </MenuItem>
      {orgSub &&
        (["project", "task"] as const).map((v) => (
          <MenuItem
            key={v}
            selected={organizeBy === v}
            onClick={() => {
              setOrganizeBy(v);
              setOrgSub(false);
            }}
            className="py-1.5 pl-[22px] text-[12.5px]"
          >
            <span className="flex-1">{v === "task" ? "By Task" : "By Project"}</span>
          </MenuItem>
        ))}
      <MenuItem
        onClick={() => {
          setOrdSub((v) => !v);
          setOrgSub(false);
        }}
      >
        <span className="flex-1">Ordering</span>
        <span className="flex items-center gap-1 text-[12.5px] text-muted-foreground">
          {ordLabel} <ChevronRight aria-hidden size={11} strokeWidth={2} />
        </span>
      </MenuItem>
      {ordSub &&
        (["updated", "name", "manual"] as const).map((o) => (
          <MenuItem
            key={o}
            selected={ordering === o}
            onClick={() => {
              setOrdering(o);
              setOrdSub(false);
            }}
            className="py-1.5 pl-[22px] text-[12.5px]"
          >
            <span className="flex-1">{o === "name" ? "Name" : o === "manual" ? "Manual Order" : "Updated"}</span>
          </MenuItem>
        ))}
    </MenuPanel>
  );
}

export function Sidebar() {
  const { projects, sessions, setFocused, focusedSession, selectProject, end } = useStore();
  const pendingCount = useStore((s) => s.pendingApprovals.length);
  const {
    pinned,
    archived,
    togglePin,
    setArchived,
    readAt,
    pinnedOrder,
    reorderPinned,
    organizeBy,
    setOrganizeBy,
    taskOrdering,
    setTaskOrdering,
    projectOrdering,
    setProjectOrdering,
  } = useUi();
  const [confirmArchive, setConfirmArchive] = useState<{ session: UiSession; reason: string } | null>(null);
  const archiveCancelRef = useRef<HTMLButtonElement>(null);
  const [archivingPk, setArchivingPk] = useState<string | null>(null);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const onPinnedDragEnd = (e: DragEndEvent) => {
    const { active, over } = e;
    if (over && active.id !== over.id) reorderPinned(String(active.id), String(over.id));
  };

  // Archive = real teardown: end the session (interrupt + stop the agent,
  // kill its terminals, remove the worktree and its ryuzi/* branch), then
  // hide the row. Work that teardown would destroy — uncommitted changes OR
  // commits that exist only on the session branch — gets a confirmation.
  // The row is archived ONLY when the backend teardown succeeded.
  const finishArchive = async (s: UiSession) => {
    setArchivingPk(s.sessionPk);
    try {
      // Shells opened for this session hold their cwd inside the worktree;
      // kill them first or the directory removal fails on Windows. Terminals
      // are always local Cockpit-process PTYs (never runner-scoped).
      await commands.termCloseSession(s.sessionPk);
      const ok = await end(s.runnerId, s.sessionPk);
      if (!ok) return; // end() already toasted; leave the row visible
      setArchived(sessionKey(s), true);
      if (isSession(s, focusedSession)) setFocused(null);
      // Drop the JS-side terminal cache only now — after teardown succeeded and
      // the drawer has unmounted (setFocused(null)). Emptying the tabs earlier
      // would let the drawer's auto-spawn open a fresh PTY into the worktree
      // being removed. termCloseSession already emitted the exit events, so on
      // the failure path above we intentionally leave the cache alone.
      useTerms.getState().disposeSession(s.runnerId, s.sessionPk);
    } finally {
      setArchivingPk(null);
      setConfirmArchive(null);
    }
  };

  const archiveSession = async (s: UiSession) => {
    if (archivingPk !== null) return;
    setArchivingPk(s.sessionPk);
    try {
      const res = await commands.worktreeDirty(s.runnerId, s.sessionPk);
      // Can't prove it's clean → treat as at-risk and ask.
      if (res.status !== "ok") {
        setConfirmArchive({ session: s, reason: "Cockpit couldn't inspect this session's worktree." });
        return;
      }
      if (res.data.dirty || res.data.unmergedCommits > 0) {
        const parts: string[] = [];
        if (res.data.dirty) parts.push("uncommitted changes");
        if (res.data.unmergedCommits > 0)
          parts.push(`${res.data.unmergedCommits} commit${res.data.unmergedCommits === 1 ? "" : "s"} that exist only on its branch`);
        setConfirmArchive({ session: s, reason: `This session's worktree still has ${parts.join(" and ")}.` });
        return;
      }
    } finally {
      // finishArchive re-acquires the guard; release it for the modal path.
      setArchivingPk(null);
    }
    await finishArchive(s);
  };
  const nav = useNav();
  const view = nav.history.current;
  const { gateways, activeGateway, setActive: setActiveGateway, loaded: gatewaysLoaded, hydrate: hydrateGateways } = useGateways();
  useEffect(() => {
    if (!gatewaysLoaded) void hydrateGateways();
  }, [gatewaysLoaded, hydrateGateways]);

  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [showArchived, setShowArchived] = useState<Record<string, boolean>>({});
  // The old Projects-header menu's "Archived" toggle was removed along with
  // the rest of that menu in this task (see OrganizeMenu above); there is no
  // UI trigger for a global archived reveal right now, so this always reads
  // false. Per-project archived reveal (the "N archived" link below) still
  // works independently of this flag.
  const [archivedGlobal] = useState(false);
  const [projectsMenuOpen, setProjectsMenuOpen] = useState(false);
  const [tasksMenuOpen, setTasksMenuOpen] = useState(false);
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [addProjectOpen, setAddProjectOpen] = useState(false);

  const q = nav.searchQuery;
  const ws = gateways.find((w) => w.id === activeGateway) ?? gateways[0];
  const projList = orderProjects(projects, projectOrdering);
  // Chat-first sessions (no project) get their own bucket above the project
  // tree — same query/archived/pin treatment as a project's session list,
  // just flat (no project to nest under).
  const qLower = q.trim().toLowerCase();
  const chatList = chatSessions(sessions)
    .filter((s) => !qLower || sessionTitle(s).toLowerCase().includes(qLower))
    .filter((s) => archivedGlobal || !archived[sessionKey(s)])
    .sort((a, b) => {
      const pin = (pinned[sessionKey(b)] ? 1 : 0) - (pinned[sessionKey(a)] ? 1 : 0);
      if (pin !== 0) return pin;
      return (b.lastActive ?? 0) - (a.lastActive ?? 0);
    });
  // Temporary shim until Task 6 rewires the lists onto visibleTasks/orderTasks —
  // sessionsForProject still expects a SessionFilterCtx, but the Sidebar no
  // longer tracks status/unread filters itself.
  const inertFilter = { statuses: {}, unreadOnly: false, readAt, focusedSession };

  const openSession = (s: UiSession) => {
    setFocused(refOf(s));
    nav.navigate({ kind: "session" });
  };

  /** Non-local runners get a small chip next to the title so multi-runner
   *  sessions are distinguishable in a merged sidebar. */
  const runnerLabel = (runnerId: string): string | null => {
    if (runnerId === LOCAL_RUNNER) return null;
    return gateways.find((g) => g.id === runnerId)?.name ?? runnerId;
  };

  return (
    <div
      className="flex min-h-0 shrink-0 flex-col overflow-hidden bg-transparent text-sidebar-foreground transition-[width] duration-200"
      style={{ width: nav.sidebarOpen ? 260 : 0 }}
    >
      {/* Primary navigation */}
      <div className="box-border flex w-[260px] flex-col gap-[2px] px-2.5 pb-1 pt-3">
        {NAV.map((item) => {
          const active = item.group.includes(view.kind);
          const Icon = item.icon;
          return (
            <Button
              key={item.label}
              type="button"
              variant="ghost"
              onClick={() => nav.navigate(item.view)}
              className={`h-auto w-full justify-start gap-2.5 rounded-md py-[7px] text-left text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-foreground dark:hover:bg-sidebar-accent ${active ? "bg-sidebar-accent" : ""}`}
            >
              <Icon aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
              {item.label}
              {item.view.kind === "inbox" && pendingCount > 0 && (
                <Badge variant="secondary" className="ml-auto h-4 min-w-4 px-1 text-[10px]">
                  {pendingCount}
                </Badge>
              )}
            </Button>
          );
        })}
      </div>

      {/* Chat sessions — chat-first sessions with no project, bucketed apart
          from the project tree below so they never look like they belong to
          whichever project happens to be first. */}
      {chatList.length > 0 && (
        <div className="box-border flex w-[260px] flex-col gap-px px-2.5">
          <div className="flex items-center gap-[2px] py-2 pl-2.5 pr-1.5">
            <div className="relative flex flex-1 items-center gap-[2px]">
              <span className="flex-1 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Tasks</span>
              <span className="relative">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className={iconBtn}
                  title="Sort and organize"
                  onClick={() => setTasksMenuOpen((v) => !v)}
                >
                  <ListFilter aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
                </Button>
              </span>
              <OrganizeMenu
                open={tasksMenuOpen}
                onClose={() => setTasksMenuOpen(false)}
                organizeBy={organizeBy}
                setOrganizeBy={setOrganizeBy}
                ordering={taskOrdering}
                setOrdering={setTaskOrdering}
                className="left-2.5 top-8 z-[70] w-[238px]"
              />
            </div>
          </div>
          {chatList.map((s) => {
            const m = statusMeta(s.status);
            const key = sessionKey(s);
            const isActive = view.kind === "session" && isSession(s, focusedSession);
            const isPinned = !!pinned[key];
            const rLabel = runnerLabel(s.runnerId);
            return (
              <div key={key} className={`group flex min-h-7 items-stretch text-sidebar-foreground ${archived[key] ? "opacity-55" : ""}`}>
                <span
                  className={`my-px flex min-w-0 flex-1 items-center gap-2 rounded-md py-[5px] pl-2 pr-1.5 hover:bg-sidebar-accent ${isActive ? "bg-sidebar-accent" : ""}`}
                >
                  <Button
                    type="button"
                    variant="ghost"
                    onClick={() => openSession(s)}
                    className="h-auto min-w-0 flex-1 justify-start gap-2 p-0 text-left text-sidebar-foreground hover:bg-transparent hover:text-sidebar-foreground dark:hover:bg-transparent"
                  >
                    <StatusDot color={m.color} pulse={m.pulse} />
                    <span className="min-w-0 flex-1 truncate">{sessionTitle(s)}</span>
                    {rLabel && (
                      <Badge variant="secondary" className="h-4 shrink-0 px-1 text-[9.5px] font-medium">
                        {rLabel}
                      </Badge>
                    )}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    title={isPinned ? "Unpin" : "Pin"}
                    className={`size-[22px] shrink-0 rounded-sm ${isPinned ? "flex text-foreground" : "hidden text-muted-foreground group-hover:flex"}`}
                    onClick={() => togglePin(key)}
                  >
                    <Pin aria-hidden size={12} strokeWidth={2} fill={isPinned ? "currentColor" : "none"} />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    title={archived[key] ? "Restore" : "Archive — ends the session and removes its scratch dir"}
                    disabled={archivingPk === s.sessionPk}
                    className="hidden size-[22px] shrink-0 rounded-sm text-muted-foreground disabled:opacity-40 group-hover:flex"
                    onClick={() => (archived[key] ? setArchived(key, false) : void archiveSession(s))}
                  >
                    <Archive aria-hidden size={12} strokeWidth={2} />
                  </Button>
                </span>
              </div>
            );
          })}
        </div>
      )}

      {/* Projects header */}
      <div className="relative box-border flex w-[260px] items-center gap-[2px] py-3 pl-5 pr-3">
        <span className="flex-1 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Projects</span>
        <span className="relative">
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className={iconBtn}
            title="Sort and organize"
            onClick={() => setProjectsMenuOpen((v) => !v)}
          >
            <ListFilter aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
          </Button>
        </span>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className={iconBtn}
          title="New project"
          onClick={() => setAddProjectOpen(true)}
        >
          <FolderPlus aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
        </Button>

        <OrganizeMenu
          open={projectsMenuOpen}
          onClose={() => setProjectsMenuOpen(false)}
          organizeBy={organizeBy}
          setOrganizeBy={setOrganizeBy}
          ordering={projectOrdering}
          setOrdering={setProjectOrdering}
          className="left-2.5 top-8 z-[70] w-[238px]"
        />
      </div>

      {/* Projects tree */}
      <div className="box-border flex w-[260px] min-h-0 flex-1 flex-col gap-px overflow-y-auto px-2.5">
        {projList.map((p) => {
          const showArch = archivedGlobal || !!showArchived[p.projectId];
          const sess = sessionsForProject(sessions, p.projectId, q, showArch, pinned, archived, inertFilter, pinnedOrder);
          const archCount = archivedCount(sessions, p.projectId, archived);
          const open = q.trim() ? sess.length > 0 : (expanded[p.projectId] ?? true);
          return (
            <div key={p.projectId} className="flex flex-col gap-px">
              <div className="group flex items-center gap-1.5 rounded-md py-1.5 pl-2 pr-1.5 text-sidebar-foreground hover:bg-sidebar-accent">
                <Button
                  type="button"
                  variant="ghost"
                  className="h-auto min-w-0 flex-1 justify-start gap-2 p-0 text-left text-sidebar-foreground hover:bg-transparent hover:text-sidebar-foreground dark:hover:bg-transparent"
                  onClick={() => setExpanded((e) => ({ ...e, [p.projectId]: !open }))}
                >
                  {open ? (
                    <FolderOpen aria-hidden size={14} strokeWidth={2} className="size-[14px] shrink-0 text-muted-foreground" />
                  ) : (
                    <Folder aria-hidden size={14} strokeWidth={2} className="size-[14px] shrink-0 text-muted-foreground" />
                  )}
                  <span className="min-w-0 flex-1 truncate font-semibold">{projectLabel(p)}</span>
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  title="Project settings"
                  className={`${iconBtn} hidden group-hover:flex`}
                  onClick={() => nav.setProjectSettingsFor(p.projectId)}
                >
                  <Settings aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  title="New task"
                  className={`${iconBtn} hidden group-hover:flex`}
                  onClick={() => {
                    selectProject(p.projectId);
                    nav.navigate({ kind: "home" });
                    setExpanded((e) => ({ ...e, [p.projectId]: true }));
                  }}
                >
                  <Plus aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
                </Button>
              </div>
              {open && (
                <>
                  <DndContext
                    sensors={sensors}
                    collisionDetection={closestCenter}
                    modifiers={[restrictToVerticalAxis]}
                    onDragEnd={onPinnedDragEnd}
                  >
                    <SortableContext
                      items={sess.filter((s) => pinned[sessionKey(s)]).map((s) => sessionKey(s))}
                      strategy={verticalListSortingStrategy}
                    >
                      {sess.map((s, i) => {
                        const key = sessionKey(s);
                        const isActive = view.kind === "session" && isSession(s, focusedSession);
                        const isPinned = !!pinned[key];
                        const unread = isUnreadVisible(s, readAt, focusedSession);
                        const showArchivedLabel = archCount > 0 && !archivedGlobal;
                        const hasTail = i < sess.length - 1 || showArchivedLabel;
                        const rowProps = {
                          session: s,
                          isActive,
                          isPinned,
                          unread,
                          isArchived: !!archived[key],
                          hasTail,
                          archiveDisabled: archivingPk === s.sessionPk,
                          runnerLabel: runnerLabel(s.runnerId),
                          onOpen: () => openSession(s),
                          onTogglePin: () => togglePin(key),
                          onToggleArchive: () => (archived[key] ? setArchived(key, false) : void archiveSession(s)),
                        };
                        return isPinned ? <SortableSessionRow key={key} {...rowProps} /> : <SessionRow key={key} {...rowProps} />;
                      })}
                    </SortableContext>
                  </DndContext>
                  {archCount > 0 && !archivedGlobal && (
                    <Button
                      type="button"
                      variant="ghost"
                      className="h-auto min-h-6 items-stretch justify-start gap-0 rounded-sm border-0 p-0 pr-2 text-left text-[11.5px] font-normal text-muted-foreground hover:bg-transparent hover:text-foreground dark:hover:bg-transparent"
                      onClick={() => setShowArchived((m) => ({ ...m, [p.projectId]: !m[p.projectId] }))}
                    >
                      <TreeGuide tail={false} reach={1} />
                      <span className="self-center pl-[7px]">{showArchived[p.projectId] ? "Hide archived" : `${archCount} archived`}</span>
                    </Button>
                  )}
                </>
              )}
            </div>
          );
        })}
      </div>

      {/* Workspace / gateway switcher */}
      <div className="relative box-border w-[260px] shrink-0 px-2.5 py-2">
        <Button
          type="button"
          variant="ghost"
          onClick={() => setWorkspaceMenuOpen((v) => !v)}
          className={`h-auto w-full justify-start gap-2.5 rounded-md py-2 text-left text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-foreground dark:hover:bg-sidebar-accent ${workspaceMenuOpen ? "bg-sidebar-accent" : ""}`}
        >
          <span className="relative flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-sidebar-border text-muted-foreground [background:color-mix(in_oklab,var(--sidebar-accent)_90%,transparent)]">
            <Server aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
            <span
              className="absolute -bottom-0.5 -right-0.5 h-[9px] w-[9px] rounded-full border-2 border-sidebar"
              style={{ background: ws?.status === "connected" ? "#22C55E" : "#9CA3AF" }}
            />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-muted-foreground">Workspace</span>
            <span className="block truncate font-semibold">{ws?.name ?? "This PC"}</span>
          </span>
          <ChevronsUpDown aria-hidden size={14} strokeWidth={2} className="size-[14px] shrink-0 text-muted-foreground" />
        </Button>

        {workspaceMenuOpen && (
          <MenuPanel onClose={() => setWorkspaceMenuOpen(false)} className="bottom-14 left-2.5 right-2.5 z-[70]">
            <MenuSectionLabel>Gateways</MenuSectionLabel>
            {gateways.map((w) => (
              <MenuItem
                key={w.id}
                selected={w.id === activeGateway}
                onClick={() => {
                  setActiveGateway(w.id);
                  setWorkspaceMenuOpen(false);
                }}
                className={w.id === activeGateway ? "bg-accent" : ""}
              >
                <span className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-md bg-muted">
                  <span className="font-mono text-[9.5px] font-semibold tracking-[0.02em] text-muted-foreground">{w.badge}</span>
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-semibold">{w.name}</span>
                  <span className="block truncate text-[11px] text-muted-foreground">{w.detail}</span>
                </span>
                <span className="shrink-0 font-mono text-[10px] text-muted-foreground">{w.latency ?? "—"}</span>
                <StatusDot color={w.status === "connected" ? "#22C55E" : "#9CA3AF"} />
              </MenuItem>
            ))}
            <MenuSeparator />
            <MenuItem
              className="font-medium"
              onClick={() => {
                setWorkspaceMenuOpen(false);
                nav.navigate({ kind: "gateways" });
              }}
            >
              <Server aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
              Connect gateway
            </MenuItem>
            <MenuItem
              className="font-medium text-muted-foreground hover:text-accent-foreground"
              onClick={() => {
                setWorkspaceMenuOpen(false);
                nav.navigate({ kind: "gateways" });
              }}
            >
              <Settings aria-hidden size={14} strokeWidth={2} />
              Manage gateways
            </MenuItem>
          </MenuPanel>
        )}
      </div>

      <AddProjectModal open={addProjectOpen} onClose={() => setAddProjectOpen(false)} />

      {confirmArchive && (
        <Modal onClose={() => setConfirmArchive(null)} width={440} busy={archivingPk !== null} initialFocus={archiveCancelRef}>
          <ModalHeader
            leading={<Archive aria-hidden className="mt-0.5 size-4 text-muted-foreground" strokeWidth={2} />}
            title="Archive session?"
            description={confirmArchive.reason}
          />
          <ModalBody>
            <p className="text-[13px] leading-[1.55] text-foreground">“{sessionTitle(confirmArchive.session)}”</p>
            <p className="mt-1 text-[12.5px] leading-[1.55] text-muted-foreground">
              Archiving ends the session and deletes the worktree and its{" "}
              <span className="font-mono text-xs">{confirmArchive.session.branch ?? "harness"}</span> branch — that work is discarded and
              unrecoverable. The transcript stays available.
            </p>
          </ModalBody>
          <ModalFooter>
            <Button
              ref={archiveCancelRef}
              type="button"
              variant="outline"
              disabled={archivingPk !== null}
              onClick={() => setConfirmArchive(null)}
            >
              Cancel
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={archivingPk !== null}
              onClick={() => void finishArchive(confirmArchive.session)}
            >
              {archivingPk !== null ? "Archiving…" : "Archive & discard work"}
            </Button>
          </ModalFooter>
        </Modal>
      )}
    </div>
  );
}
