// apps/cockpit/src/components/shell/Sidebar.tsx
import { useState } from "react";
import {
  Archive,
  Bot,
  CalendarClock,
  ChevronRight,
  ChevronsUpDown,
  Folder,
  FolderOpen,
  FolderPlus,
  Grip,
  LayoutGrid,
  ListFilter,
  Pencil,
  Pin,
  Plus,
  Server,
  Settings,
  SquareTerminal,
} from "lucide-react";
import { useStore } from "@/store";
import { useUi } from "@/store-ui";
import { useNav, type View } from "@/store-nav";
import { useFixtures } from "@/store-fixtures";
import { WORKSPACES } from "@/fixtures";
import { archivedCount, orderProjects, projectLabel, sessionTitle, sessionsForProject, type Ordering } from "@/lib/sidebar";
import { statusMeta } from "@/lib/status";
import { MenuItem, MenuPanel, MenuSectionLabel, MenuSeparator } from "@/components/common/MenuPanel";
import { StatusDot } from "@/components/common/bits";

const NAV: { label: string; icon: typeof Pencil; view: View; group: View["kind"][] }[] = [
  { label: "New session", icon: Pencil, view: { kind: "home" }, group: ["home"] },
  { label: "Providers", icon: Grip, view: { kind: "providers" }, group: ["providers", "providerDetail"] },
  { label: "Agents", icon: Bot, view: { kind: "agents" }, group: ["agents", "agentDetail"] },
  { label: "Scheduler", icon: CalendarClock, view: { kind: "scheduler" }, group: ["scheduler", "jobDetail", "jobNew"] },
  { label: "Apps", icon: LayoutGrid, view: { kind: "apps" }, group: ["apps", "appDetail", "registry"] },
  { label: "Settings", icon: Settings, view: { kind: "settings" }, group: ["settings"] },
];

const iconBtn =
  "flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground";

export function Sidebar() {
  const { projects, sessions, setFocused, focusedSessionPk, selectProject, addProject } = useStore();
  const { pinned, archived, togglePin, toggleArchive } = useUi();
  const nav = useNav();
  const view = nav.history.current;
  const { activeWorkspace, setActiveWorkspace } = useFixtures();

  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [showArchived, setShowArchived] = useState<Record<string, boolean>>({});
  const [archivedGlobal, setArchivedGlobal] = useState(false);
  const [ordering, setOrdering] = useState<Ordering>("updated");
  const [projectsMenuOpen, setProjectsMenuOpen] = useState(false);
  const [orderingSubOpen, setOrderingSubOpen] = useState(false);
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);

  const q = nav.searchQuery;
  const ws = WORKSPACES.find((w) => w.id === activeWorkspace) ?? WORKSPACES[0];
  const projList = orderProjects(projects, ordering);

  const openSession = (pk: string) => {
    setFocused(pk);
    nav.navigate({ kind: "session" });
  };

  return (
    <div
      className="acrylic-chrome flex min-h-0 shrink-0 flex-col overflow-hidden border-r border-sidebar-border text-sidebar-foreground transition-[width] duration-200"
      style={{ width: nav.sidebarOpen ? 260 : 0 }}
    >
      {/* Primary navigation */}
      <div className="box-border flex w-[260px] flex-col gap-[2px] px-2.5 pb-1 pt-3">
        {NAV.map((item) => {
          const active = item.group.includes(view.kind);
          const Icon = item.icon;
          return (
            <button
              key={item.label}
              type="button"
              onClick={() => nav.navigate(item.view)}
              className={`flex cursor-pointer items-center gap-2.5 rounded-md border-none px-2.5 py-[7px] text-left font-sans text-[13.5px] font-medium text-sidebar-foreground hover:bg-sidebar-accent ${active ? "bg-sidebar-accent" : "bg-transparent"}`}
            >
              <Icon aria-hidden size={15} strokeWidth={2} />
              {item.label}
            </button>
          );
        })}
      </div>

      {/* Projects header */}
      <div className="relative box-border flex w-[260px] items-center gap-[2px] py-3 pl-5 pr-3">
        <span className="flex-1 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Projects</span>
        <button
          type="button"
          className={iconBtn}
          title="Sort and filter"
          onClick={() => {
            setProjectsMenuOpen((v) => !v);
            setOrderingSubOpen(false);
          }}
        >
          <ListFilter aria-hidden size={14} strokeWidth={2} />
        </button>
        <button type="button" className={iconBtn} title="New project — open folder" onClick={() => void addProject()}>
          <FolderPlus aria-hidden size={14} strokeWidth={2} />
        </button>

        {projectsMenuOpen && (
          <MenuPanel onClose={() => setProjectsMenuOpen(false)} className="left-2.5 top-8 z-[70] w-[238px]">
            <MenuItem>
              <span className="flex-1">Grouping</span>
              <span className="flex items-center gap-1 text-[12.5px] text-muted-foreground">
                Project <ChevronRight aria-hidden size={11} strokeWidth={2} />
              </span>
            </MenuItem>
            <MenuItem onClick={() => setOrderingSubOpen((v) => !v)}>
              <span className="flex-1">Ordering</span>
              <span className="flex items-center gap-1 text-[12.5px] text-muted-foreground">
                {ordering === "name" ? "Name" : "Updated"} <ChevronRight aria-hidden size={11} strokeWidth={2} />
              </span>
            </MenuItem>
            {orderingSubOpen &&
              (["updated", "name"] as const).map((o) => (
                <MenuItem
                  key={o}
                  selected={ordering === o}
                  onClick={() => {
                    setOrdering(o);
                    setOrderingSubOpen(false);
                  }}
                  className="py-1.5 pl-[22px] text-[12.5px]"
                >
                  <span className="flex-1">{o === "name" ? "Name" : "Updated"}</span>
                </MenuItem>
              ))}
            <MenuSeparator />
            <MenuSectionLabel>Filters</MenuSectionLabel>
            {["Status", "PR", "Agent", "Source"].map((f) => (
              <MenuItem key={f}>
                <span className="flex-1">{f}</span>
                <ChevronRight aria-hidden size={11} strokeWidth={2} className="text-muted-foreground" />
              </MenuItem>
            ))}
            <MenuItem selected={archivedGlobal} onClick={() => setArchivedGlobal((v) => !v)}>
              <span className="flex-1">Archived</span>
            </MenuItem>
            <MenuSeparator />
            <MenuItem
              onClick={() => {
                setExpanded(Object.fromEntries(projects.map((p) => [p.projectId, false])));
                setProjectsMenuOpen(false);
              }}
            >
              Collapse all
            </MenuItem>
            <MenuItem onClick={() => setProjectsMenuOpen(false)}>Mark all as read</MenuItem>
          </MenuPanel>
        )}
      </div>

      {/* Projects tree */}
      <div className="box-border flex w-[260px] min-h-0 flex-1 flex-col gap-px overflow-y-auto px-2.5">
        {projList.map((p) => {
          const showArch = archivedGlobal || !!showArchived[p.projectId];
          const sess = sessionsForProject(sessions, p.projectId, q, showArch, pinned, archived);
          const archCount = archivedCount(sessions, p.projectId, archived);
          const open = q.trim() ? sess.length > 0 : (expanded[p.projectId] ?? true);
          return (
            <div key={p.projectId} className="flex flex-col gap-px">
              <div className="group flex items-center gap-1.5 rounded-md py-1.5 pl-2 pr-1.5 text-sidebar-foreground hover:bg-sidebar-accent">
                <button
                  type="button"
                  className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 border-none bg-transparent p-0 text-left text-sidebar-foreground"
                  onClick={() => setExpanded((e) => ({ ...e, [p.projectId]: !open }))}
                >
                  {open ? (
                    <FolderOpen aria-hidden size={14} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                  ) : (
                    <Folder aria-hidden size={14} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                  )}
                  <span className="min-w-0 flex-1 truncate text-[13px] font-semibold">{projectLabel(p)}</span>
                </button>
                <button
                  type="button"
                  title="Project settings"
                  className={`${iconBtn} hidden group-hover:flex`}
                  onClick={() => nav.setProjectSettingsFor(p.projectId)}
                >
                  <Settings aria-hidden size={13} strokeWidth={2} />
                </button>
                <button
                  type="button"
                  title="New session"
                  className={`${iconBtn} hidden group-hover:flex`}
                  onClick={() => {
                    selectProject(p.projectId);
                    nav.navigate({ kind: "home" });
                    setExpanded((e) => ({ ...e, [p.projectId]: true }));
                  }}
                >
                  <Plus aria-hidden size={14} strokeWidth={2} />
                </button>
              </div>
              {open && (
                <>
                  {sess.map((s) => {
                    const m = statusMeta(s.status);
                    const isActive = view.kind === "session" && s.sessionPk === focusedSessionPk;
                    const isPinned = !!pinned[s.sessionPk];
                    return (
                      <div
                        key={s.sessionPk}
                        className={`group flex min-h-7 items-center gap-2 rounded-md py-[5px] pl-[26px] pr-1.5 text-sidebar-foreground hover:bg-sidebar-accent ${isActive ? "bg-sidebar-accent" : ""} ${archived[s.sessionPk] ? "opacity-55" : ""}`}
                      >
                        <button
                          type="button"
                          onClick={() => openSession(s.sessionPk)}
                          className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 border-none bg-transparent p-0 text-left text-sidebar-foreground"
                        >
                          <StatusDot color={m.color} pulse={m.pulse} />
                          <span className="min-w-0 flex-1 truncate text-[12.5px] font-medium">{sessionTitle(s)}</span>
                        </button>
                        <button
                          type="button"
                          title={isPinned ? "Unpin" : "Pin"}
                          className={`h-[22px] w-[22px] shrink-0 cursor-pointer items-center justify-center rounded-sm border-none bg-transparent hover:bg-accent hover:text-accent-foreground ${isPinned ? "flex text-foreground" : "hidden text-muted-foreground group-hover:flex"}`}
                          onClick={() => togglePin(s.sessionPk)}
                        >
                          <Pin aria-hidden size={12} strokeWidth={2} fill={isPinned ? "currentColor" : "none"} />
                        </button>
                        <button
                          type="button"
                          title={archived[s.sessionPk] ? "Restore" : "Archive"}
                          className="hidden h-[22px] w-[22px] shrink-0 cursor-pointer items-center justify-center rounded-sm border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground group-hover:flex"
                          onClick={() => toggleArchive(s.sessionPk)}
                        >
                          <Archive aria-hidden size={12} strokeWidth={2} />
                        </button>
                      </div>
                    );
                  })}
                  {archCount > 0 && !archivedGlobal && (
                    <button
                      type="button"
                      className="cursor-pointer rounded-sm border-none bg-transparent px-2 pb-1.5 pl-[26px] pt-1 text-left text-[11.5px] text-muted-foreground hover:text-foreground"
                      onClick={() => setShowArchived((m) => ({ ...m, [p.projectId]: !m[p.projectId] }))}
                    >
                      {showArchived[p.projectId] ? "Hide archived" : `${archCount} archived`}
                    </button>
                  )}
                </>
              )}
            </div>
          );
        })}
      </div>

      {/* Workspace / gateway switcher */}
      <div className="relative box-border w-[260px] shrink-0 border-t border-sidebar-border px-2.5 py-2">
        <button
          type="button"
          onClick={() => setWorkspaceMenuOpen((v) => !v)}
          className={`flex w-full cursor-pointer items-center gap-2.5 rounded-md border-none px-2.5 py-2 text-left font-sans text-sidebar-foreground hover:bg-sidebar-accent ${workspaceMenuOpen ? "bg-sidebar-accent" : "bg-transparent"}`}
        >
          <span className="relative flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-sidebar-border text-muted-foreground [background:color-mix(in_oklab,var(--sidebar-accent)_90%,transparent)]">
            <Server aria-hidden size={15} strokeWidth={2} />
            <span
              className="absolute -bottom-0.5 -right-0.5 h-[9px] w-[9px] rounded-full border-2 border-sidebar"
              style={{ background: ws.status === "connected" ? "#22C55E" : "#9CA3AF" }}
            />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-muted-foreground">Workspace</span>
            <span className="block truncate text-[13px] font-semibold">{ws.name}</span>
          </span>
          <ChevronsUpDown aria-hidden size={14} strokeWidth={2} className="shrink-0 text-muted-foreground" />
        </button>

        {workspaceMenuOpen && (
          <MenuPanel onClose={() => setWorkspaceMenuOpen(false)} className="bottom-14 left-2.5 right-2.5 z-[70]">
            <MenuSectionLabel>Gateways</MenuSectionLabel>
            {WORKSPACES.map((w) => (
              <MenuItem
                key={w.id}
                selected={w.id === activeWorkspace}
                onClick={() => {
                  setActiveWorkspace(w.id);
                  setWorkspaceMenuOpen(false);
                }}
                className={w.id === activeWorkspace ? "bg-accent" : ""}
              >
                <span className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-md bg-muted">
                  <span className="font-mono text-[9.5px] font-semibold tracking-[0.02em] text-muted-foreground">{w.badge}</span>
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-semibold">{w.name}</span>
                  <span className="block truncate text-[11px] text-muted-foreground">{w.detail}</span>
                </span>
                <StatusDot color={w.status === "connected" ? "#22C55E" : "#9CA3AF"} />
              </MenuItem>
            ))}
            <MenuSeparator />
            <MenuItem className="font-medium">
              <Server aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
              Connect gateway
            </MenuItem>
            <MenuItem className="font-medium">
              <SquareTerminal aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
              Connect SSH
            </MenuItem>
            <MenuItem className="font-medium">
              <SquareTerminal aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
              Connect WSL
            </MenuItem>
            <MenuItem className="font-medium text-muted-foreground hover:text-accent-foreground">
              <Settings aria-hidden size={14} strokeWidth={2} />
              Manage gateways
            </MenuItem>
          </MenuPanel>
        )}
      </div>
    </div>
  );
}
