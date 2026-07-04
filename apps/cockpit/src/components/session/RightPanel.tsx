import { useEffect, useState } from "react";
import { ChevronRight, FileText, Maximize2, Search, SquareCheck, X } from "lucide-react";
import { useUi } from "@/store-ui";
import { useNav, type RightTab } from "@/store-nav";
import { commands } from "@/bindings";
import { diffLineStyle, parseUnifiedDiff, type ReviewFile } from "@/lib/diff";
import { basename } from "@/lib/paths";
import { FileViewer } from "@/components/FileViewer";
import { FileTreePane } from "@/components/FileTreePane";
import { DiffStat } from "@/components/common/bits";

const toolBtn =
  "flex h-[30px] w-[30px] cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground";

export function RightPanel({ sessionPk, branch, running }: { sessionPk: string; branch: string | null; running: boolean }) {
  const nav = useNav();
  const ui = useUi();
  const [reviewFile, setReviewFile] = useState(0);
  const [pathDraft, setPathDraft] = useState("");
  const [treeFilter, setTreeFilter] = useState("");
  const [reviewFiles, setReviewFiles] = useState<ReviewFile[]>([]);
  const [reviewLoading, setReviewLoading] = useState(false);
  const [reviewError, setReviewError] = useState<string | null>(null);

  const activeFileTab = ui.tabs.find((t) => t.id === ui.activeTabId) ?? ui.tabs[0];

  // Real diff of the session worktree; refreshed when the Review tab opens
  // and whenever the running turn finishes (the agent may have edited files).
  useEffect(() => {
    if (!sessionPk || !nav.rightOpen || nav.rightTab !== "review" || running) return;
    let cancelled = false;
    setReviewLoading(true);
    void commands.gitDiff(sessionPk).then((res) => {
      if (cancelled) return;
      setReviewLoading(false);
      if (res.status === "ok") {
        setReviewFiles(parseUnifiedDiff(res.data));
        setReviewError(null);
        setReviewFile(0);
      } else {
        setReviewError(res.error.message);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [sessionPk, nav.rightOpen, nav.rightTab, running]);

  const rightTabs: { id: RightTab; label: string; icon: typeof SquareCheck }[] = [
    { id: "review", label: "Review", icon: SquareCheck },
    { id: "file", label: activeFileTab ? activeFileTab.title : "Files", icon: FileText },
  ];

  const review = reviewFiles.length > 0 ? reviewFiles[Math.min(reviewFile, reviewFiles.length - 1)] : null;
  const reviewAdd = reviewFiles.reduce((n, f) => n + f.add, 0);
  const reviewDel = reviewFiles.reduce((n, f) => n + f.del, 0);

  return (
    <div className="acrylic-panel flex w-[46%] max-w-[660px] shrink-0 flex-col border-l border-border">
      {/* Tab bar */}
      <div className="box-border flex h-[55px] shrink-0 items-center gap-1 border-b border-border px-2.5">
        {rightTabs.map((t) => {
          const sel = nav.rightTab === t.id;
          const Icon = t.icon;
          return (
            <button
              key={t.id}
              type="button"
              onClick={() => nav.setRightTab(t.id)}
              className={`flex h-[30px] cursor-pointer items-center gap-[7px] whitespace-nowrap rounded-md border px-3 font-sans text-[12.5px] font-medium hover:bg-accent hover:text-accent-foreground ${
                sel ? "border-border bg-background text-foreground" : "border-transparent bg-transparent text-muted-foreground"
              }`}
            >
              <Icon aria-hidden size={13} strokeWidth={2} />
              {t.label}
            </button>
          );
        })}
        <div className="flex-1" />
        <button type="button" title="Expand" className={`${toolBtn} h-7 w-7`}>
          <Maximize2 aria-hidden size={13} strokeWidth={2} />
        </button>
      </div>

      {/* Review tab — the worktree's real git diff */}
      {nav.rightTab === "review" && (
        <>
          <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-4 py-2.5">
            <span className="font-mono text-xs text-muted-foreground">main → {branch ?? "worktree"}</span>
            <DiffStat add={reviewAdd} del={reviewDel} className="ml-auto" />
          </div>
          {reviewFiles.length > 0 && (
            <div className="flex shrink-0 gap-1 overflow-x-auto border-b border-border px-3 py-2">
              {reviewFiles.map((f, i) => (
                <button
                  key={`${f.dir}${f.name}`}
                  type="button"
                  onClick={() => setReviewFile(i)}
                  className={`flex h-7 cursor-pointer items-center gap-[7px] whitespace-nowrap rounded-md border px-2.5 font-mono text-[11.5px] text-foreground hover:bg-accent ${
                    i === reviewFile ? "border-border bg-background" : "border-transparent bg-transparent"
                  }`}
                >
                  {f.name}
                  <DiffStat add={f.add} del={f.del} className="text-[11.5px]" />
                </button>
              ))}
            </div>
          )}
          <div className="min-h-0 flex-1 overflow-auto py-2 font-mono text-xs leading-[1.7]">
            {reviewError && <div className="px-4 py-3 text-[12.5px] text-destructive">{reviewError}</div>}
            {!reviewError && !review && (
              <div className="px-4 py-3 font-sans text-[12.5px] text-muted-foreground">
                {reviewLoading ? "Reading diff…" : "No changes in the worktree yet."}
              </div>
            )}
            {review?.lines.map((l, i) => {
              const s = diffLineStyle(l);
              return (
                <div key={`${i}-${l[2]}`} className="flex" style={{ background: s.bg, color: s.color }}>
                  <span className="w-11 shrink-0 select-none pr-3 text-right text-[11px] text-code-number" style={{ background: s.numBg }}>
                    {l[1]}
                  </span>
                  <span className="w-4 shrink-0 select-none" style={{ color: s.signColor }}>
                    {s.sign}
                  </span>
                  <span className="whitespace-pre pr-4">{l[2]}</span>
                </div>
              );
            })}
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
                <button
                  type="button"
                  title="Close file"
                  className={`${toolBtn} ml-auto h-6 w-6`}
                  onClick={() => ui.closeTab(activeFileTab.id)}
                >
                  <X aria-hidden size={12} strokeWidth={2} />
                </button>
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
                <input
                  value={pathDraft}
                  onChange={(e) => setPathDraft(e.target.value)}
                  placeholder="Open file by absolute path"
                  className="flex-1 border-none bg-transparent font-mono text-[11.5px] text-foreground"
                />
              </form>
            )}
          </div>
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
              <div className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 text-xs text-muted-foreground [background:color-mix(in_oklab,var(--background)_45%,transparent)]">
                <Search aria-hidden size={12} strokeWidth={2} />
                <input
                  value={treeFilter}
                  onChange={(e) => setTreeFilter(e.target.value)}
                  placeholder="Filter files"
                  className="min-w-0 flex-1 border-none bg-transparent font-sans text-xs text-foreground outline-none"
                />
              </div>
              <FileTreePane sessionPk={sessionPk} filter={treeFilter} />
            </div>
          </div>
        </>
      )}
    </div>
  );
}
