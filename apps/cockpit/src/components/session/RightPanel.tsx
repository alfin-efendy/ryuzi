import { useEffect, useRef, useState } from "react";
import { ChevronRight, FileText, Maximize2, Minimize2, RotateCw, Search, SquareCheck, X } from "lucide-react";
import { useUi } from "@/store-ui";
import { useNav, type RightTab, clampPanelSize, RIGHT_WIDTH } from "@/store-nav";
import { useDiff, reviewFileIndex, EMPTY } from "@/store-diff";
import { commands } from "@/bindings";
import { diffLineStyle, type ReviewFile } from "@/lib/diff";
import { basename, joinPath } from "@/lib/paths";
import { Button, Input } from "@ryuzi/ui";
import { FileViewer } from "@/components/FileViewer";
import { FileTreePane } from "@/components/FileTreePane";
import { DiffStat } from "@/components/common/bits";
import { PanelResizeHandle } from "@/components/common/PanelResizeHandle";

export function RightPanel({ sessionPk, branch, running }: { sessionPk: string; branch: string | null; running: boolean }) {
  const nav = useNav();
  const ui = useUi();
  const [reviewFile, setReviewFile] = useState(0);
  const [pathDraft, setPathDraft] = useState("");
  const [treeFilter, setTreeFilter] = useState("");
  const [treeRefresh, setTreeRefresh] = useState(0);
  const diff = useDiff((s) => s.bySession[sessionPk]) ?? EMPTY;
  const fetchDiff = useDiff((s) => s.fetch);
  const pendingReview = useDiff((s) => s.pendingReview);
  const setPendingReview = useDiff((s) => s.setPendingReview);

  const activeFileTab = ui.tabs.find((t) => t.id === ui.activeTabId) ?? ui.tabs[0];

  // Auto-refresh the file tree when a running turn ends — the agent may have
  // created or removed files while it was running.
  const prevRunning = useRef(running);
  useEffect(() => {
    if (prevRunning.current && !running) setTreeRefresh((n) => n + 1);
    prevRunning.current = running;
  }, [running]);

  // Auto-fetch when the tab opens and when a running turn finishes.
  useEffect(() => {
    if (!nav.rightOpen || nav.rightTab !== "review" || running) return;
    void fetchDiff(sessionPk);
  }, [nav.rightOpen, nav.rightTab, running, fetchDiff, sessionPk]);

  // Consume a pending jump from a transcript edit card: select the file once
  // it appears in this session's diff, then clear the intent. A pending jump
  // for another session is left alone — its own panel consumes it.
  useEffect(() => {
    if (pendingReview === null || pendingReview.sessionPk !== sessionPk) return;
    const idx = reviewFileIndex(diff.files, pendingReview.path);
    if (idx >= 0) {
      setReviewFile(idx);
      setPendingReview(null);
    }
  }, [pendingReview, diff.files, setPendingReview, sessionPk]);

  useEffect(() => {
    if (!nav.rightMaximized) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") nav.setRightMaximized(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [nav.rightMaximized, nav.setRightMaximized]);

  const rightTabs: { id: RightTab; label: string; icon: typeof SquareCheck }[] = [
    { id: "review", label: "Review", icon: SquareCheck },
    { id: "file", label: activeFileTab ? activeFileTab.title : "Files", icon: FileText },
  ];

  const review = diff.files.length > 0 ? diff.files[Math.min(reviewFile, diff.files.length - 1)] : null;
  const reviewAdd = diff.files.reduce((n, f) => n + f.add, 0);
  const reviewDel = diff.files.reduce((n, f) => n + f.del, 0);

  const openInFiles = async (f: ReviewFile) => {
    const res = await commands.sessionWorkdir(sessionPk);
    if (res.status !== "ok") return;
    ui.openFile(joinPath(res.data, `${f.dir}${f.name}`));
    nav.setRightTab("file");
  };

  return (
    <div
      className={`acrylic-panel relative flex shrink-0 flex-col border-l border-border ${nav.rightMaximized ? "flex-1" : ""}`}
      style={nav.rightMaximized ? undefined : { width: nav.rightWidth }}
    >
      {!nav.rightMaximized && (
        <PanelResizeHandle
          direction="x"
          onDelta={(d) => nav.setRightWidth(clampPanelSize(nav.rightWidth - d, window.innerWidth, RIGHT_WIDTH))}
          className="absolute inset-y-0 left-0 z-10"
        />
      )}
      {/* Tab bar */}
      <div className="box-border flex h-[55px] shrink-0 items-center gap-1 border-b border-border px-2.5">
        {rightTabs.map((t) => {
          const sel = nav.rightTab === t.id;
          const Icon = t.icon;
          return (
            <Button
              key={t.id}
              variant="ghost"
              onClick={() => nav.setRightTab(t.id)}
              className={sel ? "border-border bg-background text-foreground" : "text-muted-foreground"}
            >
              <Icon aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              {t.label}
            </Button>
          );
        })}
        <div className="flex-1" />
        <Button
          variant="ghost"
          size="icon-sm"
          title={nav.rightMaximized ? "Restore panel" : "Expand panel"}
          onClick={() => nav.setRightMaximized(!nav.rightMaximized)}
          className="text-muted-foreground"
        >
          {nav.rightMaximized ? (
            <Minimize2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          ) : (
            <Maximize2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          )}
        </Button>
      </div>

      {/* Review tab — the worktree's real git diff */}
      {nav.rightTab === "review" && (
        <>
          <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-4 py-2.5">
            <span className="font-mono text-xs text-muted-foreground">main → {branch ?? "worktree"}</span>
            <Button
              variant="ghost"
              size="icon-xs"
              title="Refresh diff"
              onClick={() => void fetchDiff(sessionPk)}
              className="text-muted-foreground"
            >
              <RotateCw aria-hidden size={12} strokeWidth={2} className={diff.loading ? "animate-spin" : ""} />
            </Button>
            <DiffStat add={reviewAdd} del={reviewDel} className="ml-auto" />
          </div>
          <div className="flex min-h-0 flex-1">
            <div className="flex w-[200px] shrink-0 flex-col overflow-y-auto border-r border-border py-1.5">
              {diff.files.map((f, i) => (
                <Button
                  key={`${f.dir}${f.name}`}
                  variant="ghost"
                  title={`${f.dir}${f.name}`}
                  onClick={() => setReviewFile(i)}
                  className={`h-auto w-full justify-start gap-2 rounded-none px-3 py-[5px] text-left font-mono ${i === reviewFile ? "bg-accent" : ""}`}
                >
                  <span className="min-w-0 flex-1 truncate">{f.name}</span>
                  <DiffStat add={f.add} del={f.del} className="shrink-0 text-[11px]" />
                </Button>
              ))}
              {diff.files.length === 0 && (
                <div className="px-3 py-2 font-sans text-[12px] text-muted-foreground">
                  {diff.loading ? "Reading diff…" : "No changes yet."}
                </div>
              )}
            </div>
            <div className="min-h-0 flex-1 overflow-auto py-2 font-mono text-xs leading-[1.7]">
              {diff.error && <div className="px-4 py-3 font-sans text-[12.5px] text-destructive">{diff.error}</div>}
              {!diff.error && review && (
                <>
                  <Button
                    variant="link"
                    title="Open in Files tab"
                    onClick={() => void openInFiles(review)}
                    className="mb-1 h-auto justify-start px-4 py-0 font-mono font-semibold text-foreground underline-offset-2"
                  >
                    {review.dir}
                    {review.name}
                  </Button>
                  {review.lines.map((l, i) => {
                    const s = diffLineStyle(l);
                    return (
                      <div key={`${i}-${l[2]}`} className="flex" style={{ background: s.bg, color: s.color }}>
                        <span
                          className="w-11 shrink-0 select-none pr-3 text-right text-[11px] text-code-number"
                          style={{ background: s.numBg }}
                        >
                          {l[1]}
                        </span>
                        <span className="w-4 shrink-0 select-none" style={{ color: s.signColor }}>
                          {s.sign}
                        </span>
                        <span className="whitespace-pre pr-4">{l[2]}</span>
                      </div>
                    );
                  })}
                </>
              )}
            </div>
          </div>
        </>
      )}

      {/* File tab — wired to the real readFile IPC via dock tabs */}
      {nav.rightTab === "file" && (
        <>
          <div className="flex shrink-0 items-center gap-1.5 border-b border-border px-4 py-2.5 text-xs text-muted-foreground">
            {activeFileTab ? (
              <>
                {activeFileTab.path
                  .split(/[\\/]/)
                  .slice(-3, -1)
                  .map((part) => (
                    <span key={part} className="flex items-center gap-1.5">
                      {part}
                      <ChevronRight aria-hidden size={11} strokeWidth={2} />
                    </span>
                  ))}
                <span className="font-semibold text-foreground">{basename(activeFileTab.path)}</span>
                <Button
                  variant="ghost"
                  size="icon-xs"
                  title="Close file"
                  className="ml-auto text-muted-foreground"
                  onClick={() => ui.closeTab(activeFileTab.id)}
                >
                  <X aria-hidden size={12} strokeWidth={2} />
                </Button>
              </>
            ) : (
              <form
                className="flex h-7 w-full items-center gap-2 rounded-md border border-border px-2.5 [background:color-mix(in_oklab,var(--background)_45%,transparent)]"
                onSubmit={(e) => {
                  e.preventDefault();
                  const p = pathDraft.trim();
                  if (p) ui.openFile(p);
                  setPathDraft("");
                }}
              >
                <Search aria-hidden size={12} strokeWidth={2} />
                <Input
                  value={pathDraft}
                  onChange={(e) => setPathDraft(e.target.value)}
                  placeholder="Open file by absolute path"
                  className="h-full flex-1 rounded-none border-none bg-transparent px-0 font-mono text-foreground focus-visible:ring-0 dark:bg-transparent"
                />
              </form>
            )}
          </div>
          {ui.tabs.length > 1 && (
            <div className="flex shrink-0 items-center gap-1 overflow-x-auto border-b border-border px-2 py-1.5">
              {ui.tabs.map((t) => {
                const active = t.id === (activeFileTab?.id ?? "");
                return (
                  <div
                    key={t.id}
                    className={`flex h-7 shrink-0 items-center gap-1 rounded-md border pl-2.5 pr-1 font-sans text-[12px] ${
                      active ? "border-border bg-background text-foreground" : "border-transparent text-muted-foreground hover:bg-accent"
                    }`}
                  >
                    <Button
                      variant="ghost"
                      size="xs"
                      onClick={() => ui.setActiveTab(t.id)}
                      className="h-auto p-0 text-inherit hover:bg-transparent hover:text-inherit dark:hover:bg-transparent"
                    >
                      {t.title}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      title={`Close ${t.title}`}
                      onClick={() => ui.closeTab(t.id)}
                      className="size-5 text-muted-foreground"
                    >
                      <X aria-hidden size={10} strokeWidth={2} className="size-2.5" />
                    </Button>
                  </div>
                );
              })}
            </div>
          )}
          <div className="flex min-h-0 flex-1">
            <div className="flex min-w-0 flex-1 flex-col overflow-auto text-xs">
              {activeFileTab ? (
                <FileViewer path={activeFileTab.path} />
              ) : (
                <div className="flex flex-1 items-center justify-center font-sans text-[12.5px] text-muted-foreground">
                  Select a file from the tree.
                </div>
              )}
            </div>
            <div className="flex w-[200px] shrink-0 flex-col gap-2 overflow-y-auto border-l border-border p-2.5">
              <div className="flex items-center gap-1.5">
                <div className="flex h-7 min-w-0 flex-1 items-center gap-[7px] rounded-md border border-border px-2.5 text-xs text-muted-foreground [background:color-mix(in_oklab,var(--background)_45%,transparent)]">
                  <Search aria-hidden size={12} strokeWidth={2} />
                  <Input
                    value={treeFilter}
                    onChange={(e) => setTreeFilter(e.target.value)}
                    placeholder="Filter files"
                    className="h-full flex-1 rounded-none border-none bg-transparent px-0 text-foreground focus-visible:ring-0 dark:bg-transparent"
                  />
                </div>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  title="Refresh file tree"
                  onClick={() => setTreeRefresh((n) => n + 1)}
                  className="text-muted-foreground"
                >
                  <RotateCw aria-hidden size={12} strokeWidth={2} className="size-3" />
                </Button>
              </div>
              <FileTreePane sessionPk={sessionPk} filter={treeFilter} refreshKey={treeRefresh} />
            </div>
          </div>
        </>
      )}
    </div>
  );
}
