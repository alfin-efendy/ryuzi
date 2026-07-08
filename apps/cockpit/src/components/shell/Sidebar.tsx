// apps/cockpit/src/components/shell/Sidebar.tsx
import { useEffect, useState } from "react";
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
} from "lucide-react";
import {
  Button,
  MenuPanel,
  MenuPanelItem as MenuItem,
  MenuPanelSection as MenuSectionLabel,
  MenuPanelSeparator as MenuSeparator,
  Modal,
  ModalFooter,
} from "@ryuzi/ui";
import { useStore } from "@/store";
import { useUi } from "@/store-ui";
import { useNav, type View } from "@/store-nav";
import { useGateways } from "@/store-gateways";
import { usePlugins, sidebarPlugins } from "@/store-plugins";
import { useTerms } from "@/store-terms";
import { commands, type Session } from "@/bindings";
import { archivedCount, orderProjects, projectLabel, sessionTitle, sessionsForProject, type Ordering } from "@/lib/sidebar";
import { statusMeta } from "@/lib/status";
import { pluginIcon } from "@/lib/plugin-icons";
import { StatusDot } from "@/components/common/bits";
import { AddProjectModal } from "@/components/modals/AddProjectModal";

const NAV: { label: string; icon: typeof Pencil; view: View; group: View["kind"][] }[] = [
  { label: "New session", icon: Pencil, view: { kind: "home" }, group: ["home"] },
  { label: "Models", icon: Grip, view: { kind: "models" }, group: ["models", "providerDetail", "connectionDetail"] },
  { label: "Runtime", icon: Bot, view: { kind: "runtime" }, group: ["runtime", "runtimeDetail"] },
  { label: "Scheduler", icon: CalendarClock, view: { kind: "scheduler" }, group: ["scheduler", "jobDetail", "jobNew"] },
  { label: "Apps", icon: LayoutGrid, view: { kind: "apps" }, group: ["apps", "appDetail", "registry"] },
  { label: "Settings", icon: Settings, view: { kind: "settings" }, group: ["settings"] },
];

// Layout-only overrides for the tiny ghost icon Buttons in tree rows.
const iconBtn = "shrink-0 rounded-sm text-muted-foreground";

const guideColor = "color-mix(in srgb, var(--sidebar-foreground) 20%, var(--sidebar))";

// Tree connector in front of session rows: a rounded elbow into the row, plus
// a vertical rail continuing to the next sibling. `reach` extends the lines
// past the row edges to bridge the 1px gaps between rows.
function TreeGuide({ tail, reach }: { tail: boolean; reach: number }) {
  return (
    <span aria-hidden className="relative w-6 shrink-0 self-stretch">
      <span
        className="absolute left-3.5 box-border w-[9px] rounded-bl-[7px]"
        style={{
          top: -reach,
          height: `calc(50% + ${reach}px)`,
          borderLeft: `1.5px solid ${guideColor}`,
          borderBottom: `1.5px solid ${guideColor}`,
        }}
      />
      {tail && (
        <span
          className="absolute left-3.5 box-border w-[9px]"
          style={{ top: -reach, bottom: -reach, borderLeft: `1.5px solid ${guideColor}` }}
        />
      )}
    </span>
  );
}

export function Sidebar() {
  const { projects, sessions, setFocused, focusedSessionPk, selectProject, end } = useStore();
  const { pinned, archived, togglePin, setArchived } = useUi();
  const [confirmArchive, setConfirmArchive] = useState<{ session: Session; reason: string } | null>(null);
  const [archivingPk, setArchivingPk] = useState<string | null>(null);

  // Archive = real teardown: end the session (interrupt + stop the agent,
  // kill its terminals, remove the worktree and its harness/* branch), then
  // hide the row. Work that teardown would destroy — uncommitted changes OR
  // commits that exist only on the session branch — gets a confirmation.
  // The row is archived ONLY when the backend teardown succeeded.
  const finishArchive = async (s: Session) => {
    setArchivingPk(s.sessionPk);
    try {
      // Shells opened for this session hold their cwd inside the worktree;
      // kill them first or the directory removal fails on Windows.
      await commands.termCloseSession(s.sessionPk);
      const ok = await end(s.sessionPk);
      if (!ok) return; // end() already toasted; leave the row visible
      setArchived(s.sessionPk, true);
      if (focusedSessionPk === s.sessionPk) setFocused(null);
      // Drop the JS-side terminal cache only now — after teardown succeeded and
      // the drawer has unmounted (setFocused(null)). Emptying the tabs earlier
      // would let the drawer's auto-spawn open a fresh PTY into the worktree
      // being removed. termCloseSession already emitted the exit events, so on
      // the failure path above we intentionally leave the cache alone.
      useTerms.getState().disposeSession(s.sessionPk);
    } finally {
      setArchivingPk(null);
      setConfirmArchive(null);
    }
  };

  const archiveSession = async (s: Session) => {
    if (archivingPk !== null) return;
    setArchivingPk(s.sessionPk);
    try {
      const res = await commands.worktreeDirty(s.sessionPk);
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

  const { plugins, loaded: pluginsLoaded, load: loadPlugins } = usePlugins();
  useEffect(() => {
    if (!pluginsLoaded) void loadPlugins();
  }, [pluginsLoaded, loadPlugins]);
  const menuPlugins = sidebarPlugins(plugins);

  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [showArchived, setShowArchived] = useState<Record<string, boolean>>({});
  const [archivedGlobal, setArchivedGlobal] = useState(false);
  const [ordering, setOrdering] = useState<Ordering>("updated");
  const [projectsMenuOpen, setProjectsMenuOpen] = useState(false);
  const [orderingSubOpen, setOrderingSubOpen] = useState(false);
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [addProjectOpen, setAddProjectOpen] = useState(false);

  const q = nav.searchQuery;
  const ws = gateways.find((w) => w.id === activeGateway) ?? gateways[0];
  const projList = orderProjects(projects, ordering);

  const openSession = (pk: string) => {
    setFocused(pk);
    nav.navigate({ kind: "session" });
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
            </Button>
          );
        })}
      </div>

      {/* Plugin-driven menu section — enabled catalog/user integrations only;
          core builtins live in the fixed NAV list above. Hidden entirely
          when no plugin qualifies so it never reserves empty space. */}
      {menuPlugins.length > 0 && (
        <div className="box-border flex w-[260px] flex-col gap-[2px] px-2.5 pb-1">
          <div className="px-2.5 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Plugins</div>
          {menuPlugins.map((plugin) => {
            const active = view.kind === "pluginDetail" && view.id === plugin.id;
            const Icon = pluginIcon(plugin.icon);
            return (
              <Button
                key={plugin.id}
                type="button"
                variant="ghost"
                onClick={() => nav.navigate({ kind: "pluginDetail", id: plugin.id })}
                className={`h-auto w-full justify-start gap-2.5 rounded-md py-[7px] text-left text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-foreground dark:hover:bg-sidebar-accent ${active ? "bg-sidebar-accent" : ""}`}
              >
                <Icon aria-hidden size={15} strokeWidth={2} className="size-[15px]" />
                <span className="min-w-0 flex-1 truncate">{plugin.name}</span>
              </Button>
            );
          })}
        </div>
      )}

      {/* Projects header */}
      <div className="relative box-border flex w-[260px] items-center gap-[2px] py-3 pl-5 pr-3">
        <span className="flex-1 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Projects</span>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className={iconBtn}
          title="Sort and filter"
          onClick={() => {
            setProjectsMenuOpen((v) => !v);
            setOrderingSubOpen(false);
          }}
        >
          <ListFilter aria-hidden size={14} strokeWidth={2} className="size-[14px]" />
        </Button>
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
                  title="New session"
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
                  {sess.map((s, i) => {
                    const m = statusMeta(s.status);
                    const isActive = view.kind === "session" && s.sessionPk === focusedSessionPk;
                    const isPinned = !!pinned[s.sessionPk];
                    const showArchivedLabel = archCount > 0 && !archivedGlobal;
                    const hasTail = i < sess.length - 1 || showArchivedLabel;
                    return (
                      <div
                        key={s.sessionPk}
                        className={`group flex min-h-7 items-stretch text-sidebar-foreground ${archived[s.sessionPk] ? "opacity-55" : ""}`}
                      >
                        <TreeGuide tail={hasTail} reach={3} />
                        <span
                          className={`my-px flex min-w-0 flex-1 items-center gap-2 rounded-md py-[5px] pl-[7px] pr-1.5 hover:bg-sidebar-accent ${isActive ? "bg-sidebar-accent" : ""}`}
                        >
                          <Button
                            type="button"
                            variant="ghost"
                            onClick={() => openSession(s.sessionPk)}
                            className="h-auto min-w-0 flex-1 justify-start gap-2 p-0 text-left text-sidebar-foreground hover:bg-transparent hover:text-sidebar-foreground dark:hover:bg-transparent"
                          >
                            <StatusDot color={m.color} pulse={m.pulse} />
                            <span className="min-w-0 flex-1 truncate">{sessionTitle(s)}</span>
                          </Button>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-xs"
                            title={isPinned ? "Unpin" : "Pin"}
                            className={`size-[22px] shrink-0 rounded-sm ${isPinned ? "flex text-foreground" : "hidden text-muted-foreground group-hover:flex"}`}
                            onClick={() => togglePin(s.sessionPk)}
                          >
                            <Pin aria-hidden size={12} strokeWidth={2} fill={isPinned ? "currentColor" : "none"} />
                          </Button>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-xs"
                            title={archived[s.sessionPk] ? "Restore" : "Archive — ends the session and removes its worktree"}
                            disabled={archivingPk === s.sessionPk}
                            className="hidden size-[22px] shrink-0 rounded-sm text-muted-foreground disabled:opacity-40 group-hover:flex"
                            onClick={() => (archived[s.sessionPk] ? setArchived(s.sessionPk, false) : void archiveSession(s))}
                          >
                            <Archive aria-hidden size={12} strokeWidth={2} />
                          </Button>
                        </span>
                      </div>
                    );
                  })}
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
        <Modal onClose={() => setConfirmArchive(null)} width={440}>
          <div className="mb-1 flex items-center gap-2.5">
            <Archive aria-hidden size={16} strokeWidth={2} className="text-muted-foreground" />
            <span className="text-[15px] font-semibold tracking-[-0.01em]">Archive session?</span>
          </div>
          <p className="mb-1 mt-2 text-[13px] leading-[1.55] text-foreground">“{sessionTitle(confirmArchive.session)}”</p>
          <p className="mb-[18px] mt-1 text-[12.5px] leading-[1.55] text-muted-foreground">
            {confirmArchive.reason} Archiving ends the session and deletes the worktree and its{" "}
            <span className="font-mono text-xs">{confirmArchive.session.branch ?? "harness"}</span> branch — that work is discarded and
            unrecoverable. The transcript stays available.
          </p>
          <ModalFooter className="mt-0">
            <Button type="button" variant="outline" onClick={() => setConfirmArchive(null)}>
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
