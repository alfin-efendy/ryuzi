// apps/cockpit/src/components/shell/TitleBar.tsx
import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, ArrowRight, CornerDownLeft, FileText, PanelLeft, Search, SquareTerminal } from "lucide-react";
import { useStore } from "@/store";
import type { Row } from "@/lib/transcript";
import { useUi } from "@/store-ui";
import { useNav, type View } from "@/store-nav";
import { commands } from "@/bindings";
import { SEARCH_COMMANDS } from "@/constants";
import { runtimeById, defaultRuntimeOf, useRuntimes } from "@/store-runtimes";
import { projectLabel, sessionTitle } from "@/lib/sidebar";
import { statusMeta } from "@/lib/status";
import { useClickOutside } from "@/components/common/MenuPanel";
import { StatusDot } from "@/components/common/bits";
import { WindowControls } from "./WindowControls";
import type { Session } from "@/bindings";

const isMac = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

const tool =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

const sectionLabel = "px-2.5 pb-1 pt-[7px] text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground";
const resultBtn =
  "flex w-full cursor-pointer items-start gap-2.5 rounded-md border-none bg-transparent px-2.5 py-2 text-left font-sans text-popover-foreground hover:bg-accent";

type Snippet = { pre: string; hit: string; post: string } | null;

function transcriptSnippet(rows: Row[], q: string): Snippet {
  const lower = q.toLowerCase();
  for (const row of rows) {
    const idx = row.text.toLowerCase().indexOf(lower);
    if (idx === -1) continue;
    const start = Math.max(0, idx - 32);
    return {
      pre: (start > 0 ? "…" : "") + row.text.slice(start, idx),
      hit: row.text.slice(idx, idx + q.length),
      post: row.text.slice(idx + q.length, idx + q.length + 60),
    };
  }
  return null;
}

export function TitleBar() {
  const history = useNav((s) => s.history);
  const nav = useNav();
  const { goBack, goForward, toggleSidebar, searchQuery, setSearchQuery } = nav;
  const { projects, sessions, transcripts, setFocused } = useStore();
  const runtimes = useRuntimes((s) => s.runtimes);
  const sessionAgent = runtimeById(runtimes, nav.composerAgent) ?? defaultRuntimeOf(runtimes);
  const ui = useUi();
  const searchRef = useRef<HTMLInputElement>(null);
  const [searchFocused, setSearchFocused] = useState(false);
  const paletteRef = useClickOutside(() => setSearchFocused(false));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        searchRef.current?.focus();
        setSearchFocused(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const q = searchQuery.trim();
  const searchOpen = searchFocused && q.length > 0;

  const sessionHits = useMemo(() => {
    if (!q) return [];
    const lower = q.toLowerCase();
    return sessions
      .map((s) => {
        const titleHit = sessionTitle(s).toLowerCase().includes(lower);
        const snippet = transcriptSnippet(transcripts[s.sessionPk] ?? [], q);
        return { session: s, titleHit, snippet };
      })
      .filter((r) => r.titleHit || r.snippet)
      .slice(0, 5);
  }, [q, sessions, transcripts]);

  const fileHits = useMemo(() => {
    if (!q) return [];
    const lower = q.toLowerCase();
    return ui.tabs.filter((t) => t.path.toLowerCase().includes(lower)).slice(0, 4);
  }, [q, ui.tabs]);

  // Live filename search over the active project's workdir.
  const searchProject = useMemo(() => {
    const focused = sessions.find((s) => s.sessionPk === useStore.getState().focusedSessionPk);
    return projects.find((p) => p.projectId === focused?.projectId) ?? projects[0];
  }, [sessions, projects]);
  const [projectFileHits, setProjectFileHits] = useState<string[]>([]);
  useEffect(() => {
    if (!q || !searchProject) {
      setProjectFileHits([]);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      void commands.searchFiles(searchProject.projectId, q).then((res) => {
        if (!cancelled && res.status === "ok") setProjectFileHits(res.data.slice(0, 6));
      });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [q, searchProject]);

  const cmdHits = useMemo(() => {
    if (!q) return [];
    const lower = q.toLowerCase();
    return SEARCH_COMMANDS.filter((c) => c.label.toLowerCase().includes(lower) || c.keywords.includes(lower)).slice(0, 4);
  }, [q]);

  const empty = sessionHits.length === 0 && fileHits.length === 0 && cmdHits.length === 0;

  const closePalette = () => {
    setSearchFocused(false);
    setSearchQuery("");
  };

  const openSessionHit = (s: Session) => {
    setFocused(s.sessionPk);
    nav.navigate({ kind: "session" });
    closePalette();
  };

  const runCommand = (id: string) => {
    const target: Record<string, View> = {
      "new-session": { kind: "home" },
      gateways: { kind: "gateways" },
      models: { kind: "models" },
      scheduler: { kind: "jobNew" },
      settings: { kind: "settings" },
    };
    if (id === "toggle-terminal") nav.toggleBottom();
    else if (id === "toggle-right") nav.toggleRight();
    else if (target[id]) nav.navigate(target[id]);
    closePalette();
  };

  const canBack = history.back.length > 0;
  const canForward = history.forward.length > 0;

  return (
    <div
      data-tauri-drag-region="deep"
      className={`relative z-20 flex h-11 shrink-0 select-none items-center gap-2.5 bg-transparent ${isMac ? "pl-[78px]" : "pl-3.5"} pr-3.5`}
    >
      <div className="flex items-center gap-0.5">
        <button type="button" title="Toggle sidebar" aria-label="Toggle sidebar" className={tool} onClick={toggleSidebar}>
          <PanelLeft aria-hidden size={15} strokeWidth={2} />
        </button>
        <button
          type="button"
          title="Back"
          aria-label="Back"
          className={`${tool} ${canBack ? "text-foreground" : "cursor-default opacity-50 hover:bg-transparent"}`}
          onClick={goBack}
          disabled={!canBack}
        >
          <ArrowLeft aria-hidden size={15} strokeWidth={2} />
        </button>
        <button
          type="button"
          title="Forward"
          aria-label="Forward"
          className={`${tool} ${canForward ? "text-foreground" : "cursor-default opacity-50 hover:bg-transparent"}`}
          onClick={goForward}
          disabled={!canForward}
        >
          <ArrowRight aria-hidden size={15} strokeWidth={2} />
        </button>
      </div>

      <div className="flex flex-1 justify-center">
        <div className="relative w-full max-w-[420px]" ref={paletteRef}>
          <div className="box-border flex h-[30px] items-center gap-2 rounded-md border border-border px-3 text-muted-foreground [background:color-mix(in_oklab,var(--background)_45%,transparent)]">
            <Search aria-hidden size={13} strokeWidth={2} />
            <input
              ref={searchRef}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              onFocus={() => setSearchFocused(true)}
              onKeyDown={(e) => {
                if (e.key === "Escape") closePalette();
              }}
              placeholder="Search sessions, files, commands"
              className="flex-1 border-none bg-transparent font-sans text-[12.5px] text-foreground"
            />
            <kbd className="rounded-sm border border-border px-[5px] py-px font-mono text-[10.5px] text-muted-foreground">
              {isMac ? "⌘K" : "Ctrl K"}
            </kbd>
          </div>

          {searchOpen && (
            <div className="absolute left-1/2 top-[38px] z-[90] max-h-[460px] w-[560px] -translate-x-1/2 overflow-y-auto rounded-lg border border-border bg-popover p-1.5 text-popover-foreground shadow-2xl">
              {empty && <div className="px-3 py-3.5 text-[13px] text-muted-foreground">No results found.</div>}
              {sessionHits.length > 0 && (
                <>
                  <div className={sectionLabel}>Sessions</div>
                  {sessionHits.map(({ session: s, snippet }) => {
                    const m = statusMeta(s.status);
                    const project = projects.find((p) => p.projectId === s.projectId);
                    return (
                      <button key={s.sessionPk} type="button" className={resultBtn} onClick={() => openSessionHit(s)}>
                        <StatusDot color={m.color} pulse={m.pulse} className="mt-[5px]" />
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-[13px] font-medium">{sessionTitle(s)}</span>
                          {snippet && (
                            <span className="mt-px block truncate text-[11.5px] text-muted-foreground">
                              {snippet.pre}
                              <span className="font-semibold text-foreground">{snippet.hit}</span>
                              {snippet.post}
                            </span>
                          )}
                        </span>
                        <span className="shrink-0 text-[11px] text-muted-foreground">
                          {project ? projectLabel(project) : s.projectId}
                          {sessionAgent ? ` · ${sessionAgent.name}` : ""}
                        </span>
                      </button>
                    );
                  })}
                </>
              )}
              {(fileHits.length > 0 || projectFileHits.length > 0) && (
                <>
                  <div className={sectionLabel}>Files</div>
                  {fileHits.map((t) => (
                    <button
                      key={t.id}
                      type="button"
                      className={`${resultBtn} items-center`}
                      onClick={() => {
                        ui.setActiveTab(t.id);
                        nav.setRightOpen(true);
                        nav.setRightTab("file");
                        nav.navigate({ kind: "session" });
                        closePalette();
                      }}
                    >
                      <FileText aria-hidden size={13} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                      <span className="min-w-0 flex-1 truncate font-mono text-xs">{t.path}</span>
                      <span className="max-w-[180px] shrink-0 truncate text-[11px] text-muted-foreground">open tab</span>
                    </button>
                  ))}
                  {projectFileHits
                    .filter((rel) => !fileHits.some((t) => t.path.endsWith(rel)))
                    .map((rel) => (
                      <button
                        key={rel}
                        type="button"
                        className={`${resultBtn} items-center`}
                        onClick={() => {
                          if (!searchProject) return;
                          const sep = searchProject.workdir.includes("\\") ? "\\" : "/";
                          ui.openFile(`${searchProject.workdir}${sep}${rel.split("/").join(sep)}`);
                          nav.setRightOpen(true);
                          nav.setRightTab("file");
                          nav.navigate({ kind: "session" });
                          closePalette();
                        }}
                      >
                        <FileText aria-hidden size={13} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate font-mono text-xs">{rel}</span>
                        <span className="max-w-[180px] shrink-0 truncate text-[11px] text-muted-foreground">
                          {searchProject ? projectLabel(searchProject) : ""}
                        </span>
                      </button>
                    ))}
                </>
              )}
              {cmdHits.length > 0 && (
                <>
                  <div className={sectionLabel}>Commands</div>
                  {cmdHits.map((c) => (
                    <button key={c.id} type="button" className={`${resultBtn} items-center`} onClick={() => runCommand(c.id)}>
                      <SquareTerminal aria-hidden size={13} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                      <span className="flex-1 text-[13px] font-medium">{c.label}</span>
                      <CornerDownLeft aria-hidden size={12} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                    </button>
                  ))}
                </>
              )}
              <div className="mt-1 flex items-center gap-1.5 border-t border-border px-2.5 pb-1 pt-2 text-[11px] text-muted-foreground">
                <kbd className="rounded-sm border border-border px-1 font-mono text-[10px]">esc</kbd>
                to close
              </div>
            </div>
          )}
        </div>
      </div>

      <div className="flex w-[92px] shrink-0 items-center justify-end">{!isMac && <WindowControls />}</div>
    </div>
  );
}
