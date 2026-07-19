// apps/cockpit/src/components/shell/Sidebar.tsx
import { useEffect, useRef, useState } from "react";
import {
  Archive,
  Bot,
  Workflow,
  ChevronDown,
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
import { TASKS_BUCKET, useUi } from "@/store-ui";
import { useNav, type View } from "@/store-nav";
import { useGateways } from "@/store-gateways";
import { useTerms } from "@/store-terms";
import { commands } from "@/bindings";
import {
  archivedCount,
  dropTarget,
  isUnreadVisible,
  orderProjects,
  orderTasks,
  projectLabel,
  sessionTitle,
  visibleTasks,
  type Ordering,
} from "@/lib/sidebar";
import { LOCAL_RUNNER, isSession, refOf, sessionKey, type UiSession } from "@/lib/session-key";
import { StatusDot, TreeGuide } from "@/components/common/bits";
import { AddProjectModal } from "@/components/modals/AddProjectModal";
import type { SessionRowProps } from "@/components/shell/SessionRow";
import { SortableSessionRow } from "@/components/shell/SortableSessionRow";
import { SortableProjectRow } from "@/components/shell/SortableProjectRow";

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

// Collapse key for the whole Projects section (mirrors TASKS_BUCKET for the
// Tasks section). Distinct from any project id so a project named "__projects__"
// can't collide with the section's own collapse state.
const PROJECTS_SECTION = "__projects__";

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
  const { projects, sessions, setFocused, focusedSession, selectProject } = useStore();
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
    taskOrder,
    projectOrder,
    reorderTasks,
    reorderProjects,
    collapsed,
    toggleCollapsed,
  } = useUi();
  const [confirmArchive, setConfirmArchive] = useState<{ session: UiSession; reason: string } | null>(null);
  const archiveCancelRef = useRef<HTMLButtonElement>(null);
  const [archivingPk, setArchivingPk] = useState<string | null>(null);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  // A row's partition (pinned vs. manual-ordered) is decided by `dropTarget`;
  // cross-partition drops and no-ops are rejected there. Reordering unpinned
  // rows always switches that section's Ordering to Manual — dragging is an
  // explicit request for manual control.
  const onTaskDragEnd = (bucket: string, visibleKeys: string[]) => (e: DragEndEvent) => {
    const { active, over } = e;
    if (!over) return;
    const activeId = String(active.id);
    const overId = String(over.id);
    const target = dropTarget(activeId, overId, !!pinned[activeId], !!pinned[overId]);
    if (target === "pinned") reorderPinned(activeId, overId);
    else if (target === "manual") {
      const unpinned = visibleKeys.filter((k) => !pinned[k]);
      reorderTasks(bucket, activeId, overId, unpinned);
      setTaskOrdering("manual");
    }
  };
  const onProjectDragEnd = (visibleIds: string[]) => (e: DragEndEvent) => {
    const { active, over } = e;
    if (!over || active.id === over.id) return;
    reorderProjects(String(active.id), String(over.id), visibleIds);
    setProjectOrdering("manual");
  };

  // Archive preserves the durable session and its artifacts for retention;
  // backend state is authoritative while the local map remains an immediate
  // visibility cache until the next session refresh.
  const finishArchive = async (s: UiSession) => {
    setArchivingPk(s.sessionPk);
    try {
      const result = await commands.archiveSession(s.runnerId, s.sessionPk);
      if (result.status === "error") {
        return;
      }
      setArchived(sessionKey(s), true);
      if (isSession(s, focusedSession)) setFocused(null);
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

  const [showArchived, setShowArchived] = useState<Record<string, boolean>>({});
  const [projectsMenuOpen, setProjectsMenuOpen] = useState(false);
  const [tasksMenuOpen, setTasksMenuOpen] = useState(false);
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [addProjectOpen, setAddProjectOpen] = useState(false);

  const q = nav.searchQuery;
  const ws = gateways.find((w) => w.id === activeGateway) ?? gateways[0];
  const projList = orderProjects(projects, projectOrdering, projectOrder);
  // Top "Tasks" bucket. By Project: chat-first tasks only (project tasks nest
  // under their project below). By Task: every task, flat. Section-level archived
  // reveal is gone, so archived rows stay hidden here (showArchived = false);
  // per-project reveal still lives on each project row.
  const tasksScope = organizeBy === "task" ? "all" : "chat";
  const taskList = orderTasks(
    visibleTasks(sessions, tasksScope, q, false, archived),
    pinned,
    pinnedOrder,
    taskOrdering,
    taskOrder[TASKS_BUCKET] ?? [],
  );

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

  // Shared row-prop builder for every task-row render site (top Tasks bucket,
  // By-Project nested lists) so the three paths can't drift on click/pin/
  // archive wiring. `hasTail`/`showGuide` are the only per-site variance.
  const makeRowProps = (s: UiSession, opts: { hasTail: boolean; showGuide?: boolean }): SessionRowProps => {
    const key = sessionKey(s);
    return {
      session: s,
      isActive: view.kind === "session" && isSession(s, focusedSession),
      isPinned: !!pinned[key],
      unread: isUnreadVisible(s, readAt, focusedSession),
      isArchived: s.archivedAt != null || !!archived[key],
      hasTail: opts.hasTail,
      showGuide: opts.showGuide,
      archiveDisabled: archivingPk === s.sessionPk,
      runnerLabel: runnerLabel(s.runnerId),
      onOpen: () => openSession(s),
      onTogglePin: () => togglePin(key),
      onToggleArchive: () => {
        if (s.archivedAt != null || archived[key]) {
          void commands.restoreSession(s.runnerId, s.sessionPk).then((result) => {
            if (result.status === "ok") setArchived(key, false);
          });
        } else {
          void archiveSession(s);
        }
      },
    };
  };

  return (
    <div
      className="flex min-h-0 shrink-0 flex-col overflow-hidden bg-transparent text-sidebar-foreground transition-[width] duration-200"
      style={{ width: nav.sidebarOpen ? 260 : 0 }}
    >
      {/* Primary navigation. In By-Task mode the sidebar no longer nests a
          Projects tree, so a "Projects" entry is spliced in right after
          "New Task" and routes to the full-page project browser. */}
      <div className="box-border flex w-[260px] flex-col gap-[3px] px-3 pb-2 pt-3">
        {(organizeBy === "task"
          ? [
              NAV[0],
              { label: "Projects", icon: Folder, view: { kind: "projects" } as View, group: ["projects"] as View["kind"][] },
              ...NAV.slice(1),
            ]
          : NAV
        ).map((item, i) => {
          const active = item.group.includes(view.kind);
          const Icon = item.icon;
          return (
            <Button
              key={item.label}
              type="button"
              variant="ghost"
              onClick={() => nav.navigate(item.view)}
              className={`sidebar-item-enter group/nav relative h-auto w-full justify-start gap-2.5 rounded-md py-[6px] pl-2 pr-2.5 text-left text-[13px] font-medium tracking-[-0.006em] text-sidebar-foreground transition-all duration-150 ease-out hover:bg-sidebar-accent hover:text-sidebar-foreground dark:hover:bg-sidebar-accent ${active ? "bg-sidebar-accent" : "text-sidebar-foreground/85"}`}
              style={{ animationDelay: `${i * 25}ms` }}
            >
              {active && (
                <span
                  aria-hidden
                  className="absolute left-0 top-1/2 h-[15px] w-[2.5px] -translate-y-1/2 rounded-full bg-primary transition-all duration-200 ease-out"
                />
              )}
              <Icon
                aria-hidden
                size={15}
                strokeWidth={2}
                className={`size-[15px] shrink-0 transition-transform duration-150 ease-out group-hover/nav:scale-[1.04] ${active ? "text-sidebar-foreground" : "text-muted-foreground"}`}
              />
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

      {/* Tasks — the top flat bucket. By Project it holds only chat-first tasks
          (project tasks nest under their project); By Task it holds every task,
          flat. Rendered as guide-less SessionRows apart from the project tree. */}
      {taskList.length > 0 && (
        <div className={`box-border flex w-[260px] flex-col gap-px px-3 ${organizeBy === "task" ? "min-h-0 flex-1 overflow-y-auto" : ""}`}>
          <div className="relative flex items-center gap-1 pb-1 pl-2 pr-0.5 pt-3">
            <span className="flex-1 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70">Tasks</span>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className={iconBtn}
              title="Sort and organize"
              onClick={() => setTasksMenuOpen((v) => !v)}
            >
              <ListFilter aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className={iconBtn}
              title={collapsed[TASKS_BUCKET] ? "Expand Tasks" : "Collapse Tasks"}
              onClick={() => toggleCollapsed(TASKS_BUCKET)}
            >
              <ChevronDown
                aria-hidden
                size={13}
                strokeWidth={2}
                className={`size-[13px] transition-transform duration-200 [transition-timing-function:cubic-bezier(0.34,1.56,0.64,1)] ${collapsed[TASKS_BUCKET] ? "-rotate-90" : ""}`}
              />
            </Button>
            <OrganizeMenu
              open={tasksMenuOpen}
              onClose={() => setTasksMenuOpen(false)}
              organizeBy={organizeBy}
              setOrganizeBy={setOrganizeBy}
              ordering={taskOrdering}
              setOrdering={setTaskOrdering}
              className="right-2 top-8 z-[70] w-[238px]"
            />
          </div>
          {!collapsed[TASKS_BUCKET] && (
            <div className="sidebar-section-enter">
              <DndContext
                sensors={sensors}
                collisionDetection={closestCenter}
                modifiers={[restrictToVerticalAxis]}
                onDragEnd={onTaskDragEnd(
                  TASKS_BUCKET,
                  taskList.map((s) => sessionKey(s)),
                )}
              >
                <SortableContext items={taskList.map((s) => sessionKey(s))} strategy={verticalListSortingStrategy}>
                  {taskList.map((s) => (
                    <SortableSessionRow key={sessionKey(s)} {...makeRowProps(s, { hasTail: false, showGuide: false })} />
                  ))}
                </SortableContext>
              </DndContext>
            </div>
          )}
        </div>
      )}

      {/* Projects header — only in By-Project mode. In By-Task mode the nav's
          "Projects" entry replaces this whole section. */}
      {organizeBy === "project" && (
        <div className="relative box-border flex w-[260px] items-center gap-1 px-3 pb-1 pl-5 pt-3">
          <span className="flex-1 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70">Projects</span>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className={iconBtn}
            title="Sort and organize"
            onClick={() => setProjectsMenuOpen((v) => !v)}
          >
            <ListFilter aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className={iconBtn}
            title="New project"
            onClick={() => setAddProjectOpen(true)}
          >
            <FolderPlus aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className={iconBtn}
            title={collapsed[PROJECTS_SECTION] ? "Expand Projects" : "Collapse Projects"}
            onClick={() => toggleCollapsed(PROJECTS_SECTION)}
          >
            <ChevronDown
              aria-hidden
              size={13}
              strokeWidth={2}
              className={`size-[13px] transition-transform duration-200 [transition-timing-function:cubic-bezier(0.34,1.56,0.64,1)] ${collapsed[PROJECTS_SECTION] ? "-rotate-90" : ""}`}
            />
          </Button>

          <OrganizeMenu
            open={projectsMenuOpen}
            onClose={() => setProjectsMenuOpen(false)}
            organizeBy={organizeBy}
            setOrganizeBy={setOrganizeBy}
            ordering={projectOrdering}
            setOrdering={setProjectOrdering}
            className="right-2 top-8 z-[70] w-[238px]"
          />
        </div>
      )}

      {/* Projects — the nested task tree, By-Project only. In By-Task mode the
          Tasks (Chats) section above already holds every task flat and this
          section is gone; a full-page Projects view lives behind the nav entry. */}
      {organizeBy === "project" && (
        <div className="box-border flex w-[260px] min-h-0 flex-1 flex-col gap-px overflow-y-auto px-3">
          {!collapsed[PROJECTS_SECTION] && (
            <div className="sidebar-section-enter">
              <DndContext
                sensors={sensors}
                collisionDetection={closestCenter}
                modifiers={[restrictToVerticalAxis]}
                onDragEnd={onProjectDragEnd(projList.map((p) => p.projectId))}
              >
                <SortableContext items={projList.map((p) => p.projectId)} strategy={verticalListSortingStrategy}>
                  {projList.map((p) => {
                    const showArch = !!showArchived[p.projectId];
                    const sess = orderTasks(
                      visibleTasks(sessions, { projectId: p.projectId }, q, showArch, archived),
                      pinned,
                      pinnedOrder,
                      taskOrdering,
                      taskOrder[p.projectId] ?? [],
                    );
                    const archCount = archivedCount(sessions, p.projectId, archived);
                    const open = q.trim() ? sess.length > 0 : !collapsed[p.projectId];
                    return (
                      <div key={p.projectId} className="flex flex-col gap-px">
                        <SortableProjectRow id={p.projectId}>
                          {() => (
                            <div className="group flex items-center gap-2 rounded-md py-1.5 pl-2 pr-1.5 text-sidebar-foreground transition-colors duration-150 ease-out hover:bg-sidebar-accent">
                              <Button
                                type="button"
                                variant="ghost"
                                className="h-auto min-w-0 flex-1 justify-start gap-2 p-0 text-left text-[13px] text-sidebar-foreground hover:bg-transparent hover:text-sidebar-foreground dark:hover:bg-transparent"
                                onClick={() => toggleCollapsed(p.projectId)}
                              >
                                {open ? (
                                  <FolderOpen
                                    aria-hidden
                                    size={14}
                                    strokeWidth={2}
                                    className="size-[14px] shrink-0 text-muted-foreground transition-transform duration-200 [transition-timing-function:cubic-bezier(0.34,1.56,0.64,1)] group-hover:scale-[1.06]"
                                  />
                                ) : (
                                  <Folder
                                    aria-hidden
                                    size={14}
                                    strokeWidth={2}
                                    className="size-[14px] shrink-0 text-muted-foreground transition-transform duration-200 [transition-timing-function:cubic-bezier(0.34,1.56,0.64,1)] group-hover:scale-[1.06]"
                                  />
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
                                  if (collapsed[p.projectId]) toggleCollapsed(p.projectId);
                                }}
                              >
                                <Plus aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
                              </Button>
                            </div>
                          )}
                        </SortableProjectRow>
                        {open && (
                          <>
                            <DndContext
                              sensors={sensors}
                              collisionDetection={closestCenter}
                              modifiers={[restrictToVerticalAxis]}
                              onDragEnd={onTaskDragEnd(
                                p.projectId,
                                sess.map((s) => sessionKey(s)),
                              )}
                            >
                              <SortableContext items={sess.map((s) => sessionKey(s))} strategy={verticalListSortingStrategy}>
                                {sess.map((s, i) => {
                                  const showArchivedLabel = archCount > 0;
                                  const hasTail = i < sess.length - 1 || showArchivedLabel;
                                  return <SortableSessionRow key={sessionKey(s)} {...makeRowProps(s, { hasTail })} />;
                                })}
                              </SortableContext>
                            </DndContext>
                            {archCount > 0 && (
                              <Button
                                type="button"
                                variant="ghost"
                                className="h-auto min-h-6 items-stretch justify-start gap-0 rounded-sm border-0 p-0 pr-2 text-left text-[11.5px] font-normal text-muted-foreground hover:bg-transparent hover:text-foreground dark:hover:bg-transparent"
                                onClick={() => setShowArchived((m) => ({ ...m, [p.projectId]: !m[p.projectId] }))}
                              >
                                <TreeGuide tail={false} reach={1} />
                                <span className="self-center pl-[7px]">
                                  {showArchived[p.projectId] ? "Hide archived" : `${archCount} archived`}
                                </span>
                              </Button>
                            )}
                          </>
                        )}
                      </div>
                    );
                  })}
                </SortableContext>
              </DndContext>
            </div>
          )}
        </div>
      )}

      {/* Workspace / gateway switcher */}
      <div className="relative box-border w-[260px] shrink-0 px-3 py-2 pt-1">
        <Button
          type="button"
          variant="ghost"
          onClick={() => setWorkspaceMenuOpen((v) => !v)}
          className={`h-auto w-full justify-start gap-2.5 rounded-md py-2 pl-2 text-left text-sidebar-foreground transition-colors duration-150 ease-out hover:bg-sidebar-accent hover:text-sidebar-foreground dark:hover:bg-sidebar-accent ${workspaceMenuOpen ? "bg-sidebar-accent" : ""}`}
        >
          <span className="relative flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-sidebar-border text-muted-foreground [background:color-mix(in_oklab,var(--sidebar-accent)_90%,transparent)]">
            <Server aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
            <span
              className="absolute -bottom-0.5 -right-0.5 h-[9px] w-[9px] rounded-full border-2 border-sidebar"
              style={{ background: ws?.status === "connected" ? "#22C55E" : "#9CA3AF" }}
            />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-[9.5px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70">Workspace</span>
            <span className="block truncate text-[13px] font-semibold tracking-[-0.006em]">{ws?.name ?? "This PC"}</span>
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
